//! Core dithering engine: error diffusion, ordered matrices, halftone, blue noise, and Riemersma.

use std::sync::{LazyLock, OnceLock};

use image::{Rgba, RgbaImage};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::color::{ColorSpace, Rgb, srgb_u8_to_linear};
use crate::detmath;
use crate::ink::{SolidInk, apply_solid_ink};
use crate::palette::{Palette, SplitMix64};

/// All supported dithering algorithms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "kebab-case")]
pub enum Algorithm {
    /// Classic Floyd-Steinberg 4-neighbor error diffusion (balanced, detailed).
    #[default]
    #[cfg_attr(feature = "clap", value(alias = "fs"))]
    FloydSteinberg,
    /// Bill Atkinson's `MacPaint` diffusion (6/8 diffused: crisp, airy, editorial).
    Atkinson,
    /// Jarvis-Judice-Ninke 12-neighbor error diffusion (silky smooth gradients).
    #[cfg_attr(feature = "clap", value(alias = "jjn"))]
    JarvisJudiceNinke,
    /// Stucki 12-neighbor diffusion (crisper variant of JJN).
    Stucki,
    /// Burkes 2-row 7-neighbor diffusion (fast, clean midtones).
    Burkes,
    /// Sierra 3-row error diffusion (smooth photographic grain).
    Sierra,
    /// Sierra Two-Row error diffusion (clean and fast).
    SierraTwoRow,
    /// Sierra Lite 3-neighbor diffusion (minimalist airy texture).
    SierraLite,
    /// Ostromoukhov variable-coefficient diffusion (artifact-free, filmic midtones).
    Ostromoukhov,
    /// Ordered 2x2 Bayer matrix (bold micro-checkerboard).
    Bayer2,
    /// Ordered 4x4 Bayer matrix (iconic retro crosshatch).
    Bayer4,
    /// Ordered 8x8 Bayer matrix (smooth retro display dithering).
    Bayer8,
    /// Ordered 16x16 Bayer matrix (ultra-fine ordered gradations).
    Bayer16,
    /// 8x8 rotated clustered-dot print halftone screen.
    Halftone,
    /// 4x4 tight clustered-dot screen (bold zine dots).
    ClusteredDot,
    /// Diagonal engraving line screen.
    DiagonalLines,
    /// Horizontal CRT scanline screen.
    HorizontalLines,
    /// Vertical line etching screen.
    VerticalLines,
    /// Pen-and-ink multi-stage crosshatch screen.
    Crosshatch,
    /// 64x64 Void-and-Cluster blue-noise stipple (photographic, zero grid artifacts).
    BlueNoise,
    /// Interleaved Gradient Noise (Jimenez 2014 — ultra-crisp non-repeating grain).
    Ign,
    /// Riemersma Hilbert-curve space-filling error diffusion (organic, isotropic).
    Riemersma,
    /// Deterministic seeded white-noise threshold dithering.
    Random,
    /// Hard nearest-palette posterization without dithering.
    Threshold,
}

/// Static metadata for discovering algorithms via `list` / `capabilities`.
#[derive(Debug, Clone, Serialize)]
pub struct AlgorithmInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub category: &'static str,
    pub description: &'static str,
    pub best_for: &'static str,
    pub supports_serpentine: bool,
    pub supports_seed: bool,
    /// `true` when each output pixel depends only on that pixel's input (ordered screens, noise
    /// thresholds). With `--temporal locked` such algorithms leave unchanged regions identical
    /// across animation frames; error diffusion propagates any change to neighbouring pixels.
    pub temporally_stable: bool,
}

impl Algorithm {
    /// Complete ordered list of all algorithms.
    pub const ALL: &'static [Self] = &[
        Self::FloydSteinberg,
        Self::Atkinson,
        Self::JarvisJudiceNinke,
        Self::Stucki,
        Self::Burkes,
        Self::Sierra,
        Self::SierraTwoRow,
        Self::SierraLite,
        Self::Ostromoukhov,
        Self::Bayer2,
        Self::Bayer4,
        Self::Bayer8,
        Self::Bayer16,
        Self::Halftone,
        Self::ClusteredDot,
        Self::DiagonalLines,
        Self::HorizontalLines,
        Self::VerticalLines,
        Self::Crosshatch,
        Self::BlueNoise,
        Self::Ign,
        Self::Riemersma,
        Self::Random,
        Self::Threshold,
    ];

    /// Canonical CLI slug for this algorithm.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::FloydSteinberg => "floyd-steinberg",
            Self::Atkinson => "atkinson",
            Self::JarvisJudiceNinke => "jarvis-judice-ninke",
            Self::Stucki => "stucki",
            Self::Burkes => "burkes",
            Self::Sierra => "sierra",
            Self::SierraTwoRow => "sierra-two-row",
            Self::SierraLite => "sierra-lite",
            Self::Ostromoukhov => "ostromoukhov",
            Self::Bayer2 => "bayer2",
            Self::Bayer4 => "bayer4",
            Self::Bayer8 => "bayer8",
            Self::Bayer16 => "bayer16",
            Self::Halftone => "halftone",
            Self::ClusteredDot => "clustered-dot",
            Self::DiagonalLines => "diagonal-lines",
            Self::HorizontalLines => "horizontal-lines",
            Self::VerticalLines => "vertical-lines",
            Self::Crosshatch => "crosshatch",
            Self::BlueNoise => "blue-noise",
            Self::Ign => "ign",
            Self::Riemersma => "riemersma",
            Self::Random => "random",
            Self::Threshold => "threshold",
        }
    }

    /// Detailed metadata for agent discovery and human listings.
    #[must_use]
    pub const fn info(self) -> AlgorithmInfo {
        match self {
            Self::FloydSteinberg => AlgorithmInfo {
                id: "floyd-steinberg",
                name: "Floyd-Steinberg",
                category: "error-diffusion",
                description: "Classic 4-neighbor error diffusion (7/3/5/1)",
                best_for: "General-purpose photography, crisp detail, multi-color palettes",
                supports_serpentine: true,
                temporally_stable: false,
                supports_seed: false,
            },
            Self::Atkinson => AlgorithmInfo {
                id: "atkinson",
                name: "Atkinson",
                category: "error-diffusion",
                description: "Bill Atkinson's MacPaint kernel diffusing 6/8 of error for airy highlights",
                best_for: "1-bit editorial art, portraits, classic Macintosh look, high contrast",
                supports_serpentine: true,
                temporally_stable: false,
                supports_seed: false,
            },
            Self::JarvisJudiceNinke => AlgorithmInfo {
                id: "jarvis-judice-ninke",
                name: "Jarvis-Judice-Ninke",
                category: "error-diffusion",
                description: "Wide 12-neighbor diffusion across 3 rows",
                best_for: "Smooth photographic gradations and rich tonal transitions",
                supports_serpentine: true,
                temporally_stable: false,
                supports_seed: false,
            },
            Self::Stucki => AlgorithmInfo {
                id: "stucki",
                name: "Stucki",
                category: "error-diffusion",
                description: "Crisp, high-acuity 12-neighbor variant of JJN",
                best_for: "Sharp architectural/editorial prints and Risograph posters",
                supports_serpentine: true,
                temporally_stable: false,
                supports_seed: false,
            },
            Self::Burkes => AlgorithmInfo {
                id: "burkes",
                name: "Burkes",
                category: "error-diffusion",
                description: "Fast 2-row 7-neighbor diffusion with clean diagonals",
                best_for: "Clean illustrations and fast batch processing",
                supports_serpentine: true,
                temporally_stable: false,
                supports_seed: false,
            },
            Self::Sierra => AlgorithmInfo {
                id: "sierra",
                name: "Sierra",
                category: "error-diffusion",
                description: "3-row 10-neighbor diffusion with balanced fine grain",
                best_for: "Natural photography with smooth shadow transitions",
                supports_serpentine: true,
                temporally_stable: false,
                supports_seed: false,
            },
            Self::SierraTwoRow => AlgorithmInfo {
                id: "sierra-two-row",
                name: "Sierra Two-Row",
                category: "error-diffusion",
                description: "Lighter 2-row Sierra kernel with crisp structure",
                best_for: "Portraits and UI graphics",
                supports_serpentine: true,
                temporally_stable: false,
                supports_seed: false,
            },
            Self::SierraLite => AlgorithmInfo {
                id: "sierra-lite",
                name: "Sierra Lite",
                category: "error-diffusion",
                description: "Minimal 3-neighbor diffusion (2/1/1)",
                best_for: "Lightweight vintage stippling and small icons",
                supports_serpentine: true,
                temporally_stable: false,
                supports_seed: false,
            },
            Self::Ostromoukhov => AlgorithmInfo {
                id: "ostromoukhov",
                name: "Ostromoukhov",
                category: "error-diffusion",
                description: "Tone-adaptive variable-coefficient diffusion eliminating worm artifacts",
                best_for: "Gallery-quality monochrome & duotone prints, smooth gradients",
                supports_serpentine: true,
                temporally_stable: false,
                supports_seed: false,
            },
            Self::Bayer2 => AlgorithmInfo {
                id: "bayer2",
                name: "Bayer 2x2",
                category: "ordered",
                description: "Tiny 2x2 ordered threshold matrix",
                best_for: "Chunky bold retro graphics and graphic icons",
                supports_serpentine: false,
                temporally_stable: true,
                supports_seed: false,
            },
            Self::Bayer4 => AlgorithmInfo {
                id: "bayer4",
                name: "Bayer 4x4",
                category: "ordered",
                description: "Iconic 4x4 ordered crosshatch pattern",
                best_for: "Game Boy / pixel art, web hero textures, animation-stable dithers",
                supports_serpentine: false,
                temporally_stable: true,
                supports_seed: false,
            },
            Self::Bayer8 => AlgorithmInfo {
                id: "bayer8",
                name: "Bayer 8x8",
                category: "ordered",
                description: "64-level ordered Bayer screen",
                best_for: "Smooth retro displays, gradients, CRT / cyberpunk aesthetics",
                supports_serpentine: false,
                temporally_stable: true,
                supports_seed: false,
            },
            Self::Bayer16 => AlgorithmInfo {
                id: "bayer16",
                name: "Bayer 16x16",
                category: "ordered",
                description: "256-level ultra-fine ordered matrix",
                best_for: "Subtle web backgrounds and smooth ordered color ramps",
                supports_serpentine: false,
                temporally_stable: true,
                supports_seed: false,
            },
            Self::Halftone => AlgorithmInfo {
                id: "halftone",
                name: "Print Halftone",
                category: "ordered",
                description: "8x8 rotated 45-degree clustered-dot screen",
                best_for: "Newspaper, comic, zine, pop-art, and poster prints",
                supports_serpentine: false,
                temporally_stable: true,
                supports_seed: false,
            },
            Self::ClusteredDot => AlgorithmInfo {
                id: "clustered-dot",
                name: "Clustered Dot 4x4",
                category: "ordered",
                description: "Tight 4x4 clustered dot screen",
                best_for: "Bold graphic halftones at smaller resolutions",
                supports_serpentine: false,
                temporally_stable: true,
                supports_seed: false,
            },
            Self::DiagonalLines => AlgorithmInfo {
                id: "diagonal-lines",
                name: "Diagonal Lines",
                category: "ordered",
                description: "45-degree copperplate engraving line screen",
                best_for: "Woodcut, currency engraving, and stylized editorial graphics",
                supports_serpentine: false,
                temporally_stable: true,
                supports_seed: false,
            },
            Self::HorizontalLines => AlgorithmInfo {
                id: "horizontal-lines",
                name: "Horizontal Scanlines",
                category: "ordered",
                description: "Horizontal line-weight screen",
                best_for: "CRT monitors, broadcast vibes, minimalist striped art",
                supports_serpentine: false,
                temporally_stable: true,
                supports_seed: false,
            },
            Self::VerticalLines => AlgorithmInfo {
                id: "vertical-lines",
                name: "Vertical Etching",
                category: "ordered",
                description: "Vertical fluted line-weight screen",
                best_for: "Architectural posters and fluted-glass graphic textures",
                supports_serpentine: false,
                temporally_stable: true,
                supports_seed: false,
            },
            Self::Crosshatch => AlgorithmInfo {
                id: "crosshatch",
                name: "Pen Crosshatch",
                category: "ordered",
                description: "Multi-density pen-and-ink diagonal crosshatching screen",
                best_for: "Illustration, sketch, and archival bookplate looks",
                supports_serpentine: false,
                temporally_stable: true,
                supports_seed: false,
            },
            Self::BlueNoise => AlgorithmInfo {
                id: "blue-noise",
                name: "Blue Noise Stipple",
                category: "stochastic",
                description: "64x64 Void-and-Cluster tileable blue-noise mask",
                best_for: "Organic filmic stippling, modern web hero art, artifact-free gradients",
                supports_serpentine: false,
                temporally_stable: true,
                supports_seed: false,
            },
            Self::Ign => AlgorithmInfo {
                id: "ign",
                name: "Interleaved Gradient Noise",
                category: "stochastic",
                description: "High-frequency non-repeating Jimenez gradient noise",
                best_for: "Clean modern digital grain with zero tiling artifacts",
                supports_serpentine: false,
                temporally_stable: true,
                supports_seed: true,
            },
            Self::Riemersma => AlgorithmInfo {
                id: "riemersma",
                name: "Riemersma Hilbert",
                category: "stochastic",
                description: "Error diffusion along a Hilbert space-filling curve",
                best_for: "Organic painterly stipple with zero directional bias",
                supports_serpentine: false,
                temporally_stable: false,
                supports_seed: false,
            },
            Self::Random => AlgorithmInfo {
                id: "random",
                name: "Seeded White Noise",
                category: "stochastic",
                description: "Reproducible white-noise threshold dithering",
                best_for: "Gritty lo-fi photocopy / punk zine textures",
                supports_serpentine: false,
                temporally_stable: true,
                supports_seed: true,
            },
            Self::Threshold => AlgorithmInfo {
                id: "threshold",
                name: "Hard Threshold",
                category: "baseline",
                description: "Direct nearest-palette posterization with zero dithering",
                best_for: "Clean graphic stencils, silkscreen, and baseline comparisons",
                supports_serpentine: false,
                temporally_stable: true,
                supports_seed: false,
            },
        }
    }
}

/// Parameters controlling the dithering algorithm itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DitherOptions {
    pub algorithm: Algorithm,
    pub color_space: ColorSpace,
    /// Dither / error-diffusion strength in `[0.0, 1.5]` (`1.0` = standard).
    pub strength: f32,
    /// Alternate scanline direction each row for error diffusion (reduces worm artifacts).
    pub serpentine: bool,
    /// Threshold bias in `[-0.5, 0.5]` (shifts midtone balance darker/lighter).
    pub bias: f32,
    /// Extra deterministic jitter in `[0.0, 0.5]` to soften ordered patterns or break banding.
    pub noise: f32,
    /// Scale multiplier for ordered/halftone screen cells (`1` = native, `2` = 2x larger halftone dots).
    pub pattern_scale: u32,
    /// Optional `(x, y)` cell phase offset for ordered/blue-noise screens (used by animated film boil).
    #[serde(default)]
    pub pattern_offset: [u32; 2],
    /// Deterministic PRNG seed for stochastic algorithms and `--noise`.
    pub seed: u64,
    /// Solid ink, clean paper and outline settings layered over the dither (all off by default).
    #[serde(flatten)]
    pub ink: SolidInk,
}

impl Default for DitherOptions {
    fn default() -> Self {
        Self {
            algorithm: Algorithm::FloydSteinberg,
            color_space: ColorSpace::Oklab,
            strength: 1.0,
            serpentine: true,
            bias: 0.0,
            noise: 0.0,
            pattern_scale: 1,
            pattern_offset: [0, 0],
            seed: 42,
            ink: SolidInk::OFF,
        }
    }
}

/// Run the selected dithering algorithm on `img` using `palette` and `opts`.
#[must_use]
pub fn dither_image(img: &RgbaImage, palette: &Palette, opts: &DitherOptions) -> RgbaImage {
    let mut out = match opts.algorithm {
        Algorithm::FloydSteinberg => diffuse_kernel(
            img,
            palette,
            opts,
            [7.0],
            [(-1, 3.0), (0, 5.0), (1, 1.0)],
            [],
            16.0,
        ),
        Algorithm::Atkinson => diffuse_kernel(
            img,
            palette,
            opts,
            [1.0, 1.0],
            [(-1, 1.0), (0, 1.0), (1, 1.0)],
            [(0, 1.0)],
            8.0,
        ),
        Algorithm::JarvisJudiceNinke => diffuse_kernel(
            img,
            palette,
            opts,
            [7.0, 5.0],
            [(-2, 3.0), (-1, 5.0), (0, 7.0), (1, 5.0), (2, 3.0)],
            [(-2, 1.0), (-1, 3.0), (0, 5.0), (1, 3.0), (2, 1.0)],
            48.0,
        ),
        Algorithm::Stucki => diffuse_kernel(
            img,
            palette,
            opts,
            [8.0, 4.0],
            [(-2, 2.0), (-1, 4.0), (0, 8.0), (1, 4.0), (2, 2.0)],
            [(-2, 1.0), (-1, 2.0), (0, 4.0), (1, 2.0), (2, 1.0)],
            42.0,
        ),
        Algorithm::Burkes => diffuse_kernel(
            img,
            palette,
            opts,
            [8.0, 4.0],
            [(-2, 2.0), (-1, 4.0), (0, 8.0), (1, 4.0), (2, 2.0)],
            [],
            32.0,
        ),
        Algorithm::Sierra => diffuse_kernel(
            img,
            palette,
            opts,
            [5.0, 3.0],
            [(-2, 2.0), (-1, 4.0), (0, 5.0), (1, 4.0), (2, 2.0)],
            [(-1, 2.0), (0, 3.0), (1, 2.0)],
            32.0,
        ),
        Algorithm::SierraTwoRow => diffuse_kernel(
            img,
            palette,
            opts,
            [4.0, 3.0],
            [(-2, 1.0), (-1, 2.0), (0, 3.0), (1, 2.0), (2, 1.0)],
            [],
            16.0,
        ),
        Algorithm::SierraLite => {
            diffuse_kernel(img, palette, opts, [2.0], [(-1, 1.0), (0, 1.0)], [], 4.0)
        }
        Algorithm::Ostromoukhov => diffuse_ostromoukhov(img, palette, opts),
        Algorithm::Bayer2 => ordered_dither(img, palette, opts, &bayer_matrix(2), 2),
        Algorithm::Bayer4 => ordered_dither(img, palette, opts, &bayer_matrix(4), 4),
        Algorithm::Bayer8 => ordered_dither(img, palette, opts, &bayer_matrix(8), 8),
        Algorithm::Bayer16 => ordered_dither(img, palette, opts, &bayer_matrix(16), 16),
        Algorithm::Halftone => ordered_dither(img, palette, opts, &HALFTONE_8X8, 8),
        Algorithm::ClusteredDot => ordered_dither(img, palette, opts, &CLUSTERED_4X4, 4),
        Algorithm::DiagonalLines => ordered_dither(img, palette, opts, &DIAGONAL_8X8, 8),
        Algorithm::HorizontalLines => ordered_dither(img, palette, opts, &HORIZONTAL_8X8, 8),
        Algorithm::VerticalLines => ordered_dither(img, palette, opts, &VERTICAL_8X8, 8),
        Algorithm::Crosshatch => ordered_dither(img, palette, opts, &CROSSHATCH_8X8, 8),
        Algorithm::BlueNoise => ordered_dither(img, palette, opts, blue_noise_64(), 64),
        Algorithm::Ign => stochastic_dither(img, palette, opts, StochasticKind::Ign),
        Algorithm::Random => stochastic_dither(img, palette, opts, StochasticKind::Random),
        Algorithm::Riemersma => riemersma_dither(img, palette, opts),
        Algorithm::Threshold => threshold_dither(img, palette, opts),
    };
    apply_solid_ink(img, &mut out, palette, &opts.ink);
    out
}

fn pixel_to_work(px: Rgba<u8>, space: ColorSpace) -> [f32; 3] {
    match space {
        ColorSpace::Oklab | ColorSpace::Linear => [
            srgb_u8_to_linear(px[0]),
            srgb_u8_to_linear(px[1]),
            srgb_u8_to_linear(px[2]),
        ],
        ColorSpace::Srgb => [
            f32::from(px[0]) / 255.0,
            f32::from(px[1]) / 255.0,
            f32::from(px[2]) / 255.0,
        ],
    }
}

fn palette_work_color(palette: &Palette, idx: usize, space: ColorSpace) -> [f32; 3] {
    match space {
        ColorSpace::Oklab | ColorSpace::Linear => palette.linear_colors()[idx].to_array(),
        ColorSpace::Srgb => palette.srgb_colors()[idx],
    }
}

/// Columns of padding on each side of a diffusion row, so that taps near the image edge land in
/// cells nothing reads instead of needing a bounds test per tap.
const ROW_PAD: usize = 2;

/// The three rows that error diffusion can still reach: the working-space value of every pixel
/// plus the error diffused into it so far. Rows enter as the scan approaches and each cell
/// receives its additions in the same order as with a whole-image buffer, so the sums are
/// bit-identical while the memory in play stays a few kilobytes.
struct DiffusionRows<'a> {
    src: &'a [u8],
    width: usize,
    space: ColorSpace,
    current: Vec<[f32; 3]>,
    next: Vec<[f32; 3]>,
    after: Vec<[f32; 3]>,
}

impl<'a> DiffusionRows<'a> {
    fn new(img: &'a RgbaImage, space: ColorSpace) -> Self {
        let width = img.width() as usize;
        let mut rows = Self {
            src: img.as_raw(),
            width,
            space,
            current: vec![[0.0; 3]; width + 2 * ROW_PAD],
            next: vec![[0.0; 3]; width + 2 * ROW_PAD],
            after: vec![[0.0; 3]; width + 2 * ROW_PAD],
        };
        Self::load(rows.src, width, space, 0, &mut rows.current);
        Self::load(rows.src, width, space, 1, &mut rows.next);
        rows
    }

    /// Reset `row` to the working-space values of source row `y`; past the last row it is left
    /// as a scratch target.
    fn load(src: &[u8], width: usize, space: ColorSpace, y: usize, row: &mut [[f32; 3]]) {
        let Some(pixels) = src.get(y * width * 4..(y + 1) * width * 4) else {
            return;
        };
        row.fill([0.0; 3]);
        for (cell, px) in row[ROW_PAD..].iter_mut().zip(pixels.as_chunks::<4>().0) {
            *cell = pixel_to_work(Rgba(*px), space);
        }
    }

    /// Bring in row `y + 2` before row `y` is diffused.
    fn begin_row(&mut self, y: usize) {
        Self::load(self.src, self.width, self.space, y + 2, &mut self.after);
    }

    /// Row `y` is finished: the next row becomes current.
    fn end_row(&mut self) {
        std::mem::swap(&mut self.current, &mut self.next);
        std::mem::swap(&mut self.next, &mut self.after);
    }
}

#[inline]
fn add_error(cell: &mut [f32; 3], err: [f32; 3], factor: f32) {
    cell[0] += err[0] * factor;
    cell[1] += err[1] * factor;
    cell[2] += err[2] * factor;
}

/// Error diffusion with a fixed kernel: `same` holds the weights for the pixels 1, 2, ... ahead on
/// the current row, `next` and `after` the `(dx, weight)` taps on the two rows below.
fn diffuse_kernel<const SAME: usize, const NEXT: usize, const AFTER: usize>(
    img: &RgbaImage,
    palette: &Palette,
    opts: &DitherOptions,
    same: [f32; SAME],
    next: [(isize, f32); NEXT],
    after: [(isize, f32); AFTER],
    divisor: f32,
) -> RgbaImage {
    let w = img.width() as usize;
    let h = img.height() as usize;
    let src = img.as_raw();
    let mut dst = vec![0u8; w * h * 4];
    let same = same.map(|weight| weight / divisor);
    let next = next.map(|(dx, weight)| (dx, weight / divisor));
    let after = after.map(|(dx, weight)| (dx, weight / divisor));

    let strength = opts.strength.clamp(0.0, 1.5);
    let bias = opts.bias.clamp(-0.5, 0.5);
    let noise = opts.noise.clamp(0.0, 0.5);
    let mut rng = SplitMix64::new(opts.seed ^ 0xD174_E800_1234_5678);
    let palette_rgb = palette.colors();
    let matcher = palette.matcher(opts.color_space);
    let mut rows = DiffusionRows::new(img, opts.color_space);

    for y in 0..h {
        rows.begin_row(y);
        let reverse = opts.serpentine && (y % 2 == 1);
        let dir: isize = if reverse { -1 } else { 1 };
        let src_row = &src[y * w * 4..(y + 1) * w * 4];
        let out_row = &mut dst[y * w * 4..(y + 1) * w * 4];
        // Error for the very next pixel stays out of the row buffer until that pixel is read.
        // Where there is none yet it is zero: adding it can only turn -0.0 into +0.0, and no
        // palette choice depends on the sign of a zero (see `quantization_sample`).
        let mut ahead = [0.0f32; 3];
        for step in 0..w {
            let x = if reverse { w - 1 - step } else { step };
            let alpha = src_row[x * 4 + 3];
            if alpha == 0 {
                ahead = [0.0; 3];
                continue;
            }

            let i = x + ROW_PAD;
            let cell = rows.current[i];
            let bpx = [cell[0] + ahead[0], cell[1] + ahead[1], cell[2] + ahead[2]];
            let cur = quantization_sample(bpx, bias, noise, &mut rng);
            let chosen_idx = matcher.nearest(cur);
            let chosen_rgb = palette_rgb[chosen_idx];
            let chosen_work = palette_work_color(palette, chosen_idx, opts.color_space);
            out_row[x * 4..x * 4 + 4].copy_from_slice(&[
                chosen_rgb.r,
                chosen_rgb.g,
                chosen_rgb.b,
                alpha,
            ]);

            // Compute un-biased quantization error so bias doesn't accumulate unboundedly.
            let err = diffusion_error(bpx, chosen_work, strength);
            ahead = [err[0] * same[0], err[1] * same[0], err[2] * same[0]];
            for (distance, &factor) in same.iter().enumerate().skip(1) {
                let n = i.wrapping_add_signed((distance as isize + 1) * dir);
                add_error(&mut rows.current[n], err, factor);
            }
            for &(dx, factor) in &next {
                add_error(&mut rows.next[i.wrapping_add_signed(dx * dir)], err, factor);
            }
            for &(dx, factor) in &after {
                add_error(
                    &mut rows.after[i.wrapping_add_signed(dx * dir)],
                    err,
                    factor,
                );
            }
        }
        rows.end_row();
    }

    RgbaImage::from_raw(img.width(), img.height(), dst)
        .unwrap_or_else(|| RgbaImage::new(img.width(), img.height()))
}

/// The value matched against the palette: the accumulated pixel plus bias and jitter, clamped.
#[inline]
fn quantization_sample(bpx: [f32; 3], bias: f32, noise: f32, rng: &mut SplitMix64) -> [f32; 3] {
    if noise > 1e-5 {
        let jitter = (rng.next_f32() - 0.5) * noise;
        return bpx.map(|v| (v + bias + jitter).clamp(0.0, 1.0));
    }
    if bias == 0.0 {
        // `v + 0.0 + 0.0` only ever turns -0.0 into 0.0, so the two additions can be skipped.
        // The sign of a zero never changes a palette choice: every distance squares the
        // difference, -0.0 - c equals 0.0 - c, and Oklab clamps negatives to zero first. Any
        // value computed from a zero of either sign is then equal up to that sign as well.
        return bpx.map(|v| v.clamp(0.0, 1.0));
    }
    bpx.map(|v| (v + bias + 0.0).clamp(0.0, 1.0))
}

/// `(accumulated - chosen) * strength` per channel.
#[inline]
fn diffusion_error(bpx: [f32; 3], chosen: [f32; 3], strength: f32) -> [f32; 3] {
    let diff = [bpx[0] - chosen[0], bpx[1] - chosen[1], bpx[2] - chosen[2]];
    // Multiplying by exactly 1.0 returns the same bits.
    if strength.to_bits() == 1.0f32.to_bits() {
        diff
    } else {
        diff.map(|d| d * strength)
    }
}

/// Precomputed 256-entry Ostromoukhov (SIGGRAPH 2001) tone-adaptive diffusion weight LUT.
static OSTROMOUKHOV_LUT: LazyLock<[[f32; 3]; 256]> = LazyLock::new(|| {
    // Keyframes from Ostromoukhov (2001) Table 1 (level, d_right, d_down_left, d_down).
    const KEYS: &[(usize, f32, f32, f32)] = &[
        (0, 13.0, 0.0, 5.0),
        (1, 13.0, 0.0, 5.0),
        (2, 21.0, 0.0, 10.0),
        (3, 7.0, 0.0, 4.0),
        (4, 8.0, 0.0, 5.0),
        (6, 47.0, 3.0, 28.0),
        (8, 23.0, 3.0, 13.0),
        (10, 15.0, 3.0, 8.0),
        (14, 22.0, 6.0, 11.0),
        (18, 43.0, 15.0, 20.0),
        (24, 7.0, 3.0, 3.0),
        (30, 501.0, 224.0, 211.0),
        (36, 249.0, 116.0, 103.0),
        (44, 165.0, 80.0, 67.0),
        (52, 123.0, 62.0, 49.0),
        (64, 489.0, 256.0, 200.0),
        (76, 81.0, 44.0, 31.0),
        (88, 483.0, 268.0, 181.0),
        (96, 60.0, 35.0, 23.0),
        (104, 53.0, 33.0, 19.0),
        (112, 237.0, 157.0, 80.0),
        (120, 471.0, 323.0, 153.0),
        (127, 7.0, 5.0, 3.0),
    ];

    let mut table = [[0.0f32; 3]; 256];
    for (level, slot) in table.iter_mut().enumerate() {
        let i = if level > 127 { 255 - level } else { level };
        let mut lo = KEYS[0];
        let mut hi = KEYS[KEYS.len() - 1];
        for w in KEYS.windows(2) {
            if i >= w[0].0 && i <= w[1].0 {
                lo = w[0];
                hi = w[1];
                break;
            }
        }
        let span = (hi.0.saturating_sub(lo.0)).max(1) as f32;
        let t = (i.saturating_sub(lo.0)) as f32 / span;

        let lo_sum = lo.1 + lo.2 + lo.3;
        let hi_sum = hi.1 + hi.2 + hi.3;
        let w0 = (lo.1 / lo_sum) * (1.0 - t) + (hi.1 / hi_sum) * t;
        let w1 = (lo.2 / lo_sum) * (1.0 - t) + (hi.2 / hi_sum) * t;
        let w2 = (lo.3 / lo_sum) * (1.0 - t) + (hi.3 / hi_sum) * t;
        let norm = w0 + w1 + w2;
        *slot = [w0 / norm, w1 / norm, w2 / norm];
    }
    table
});

#[inline]
fn ostromoukhov_weights(level: u8) -> [f32; 3] {
    OSTROMOUKHOV_LUT[usize::from(level)]
}

fn diffuse_ostromoukhov(img: &RgbaImage, palette: &Palette, opts: &DitherOptions) -> RgbaImage {
    let w = img.width() as usize;
    let h = img.height() as usize;
    let src = img.as_raw();
    let mut dst = vec![0u8; w * h * 4];

    let strength = opts.strength.clamp(0.0, 1.5);
    let bias = opts.bias.clamp(-0.5, 0.5);
    let noise = opts.noise.clamp(0.0, 0.5);
    let mut rng = SplitMix64::new(opts.seed ^ 0x0578_0300_ABCD_EF01);
    let palette_rgb = palette.colors();
    let matcher = palette.matcher(opts.color_space);
    let mut rows = DiffusionRows::new(img, opts.color_space);

    for y in 0..h {
        rows.begin_row(y);
        let reverse = opts.serpentine && (y % 2 == 1);
        let dir: isize = if reverse { -1 } else { 1 };
        let src_row = &src[y * w * 4..(y + 1) * w * 4];
        let out_row = &mut dst[y * w * 4..(y + 1) * w * 4];
        let mut ahead = [0.0f32; 3];
        for step in 0..w {
            let x = if reverse { w - 1 - step } else { step };
            let px = &src_row[x * 4..x * 4 + 4];
            let alpha = px[3];
            if alpha == 0 {
                ahead = [0.0; 3];
                continue;
            }

            let i = x + ROW_PAD;
            let cell = rows.current[i];
            let bpx = [cell[0] + ahead[0], cell[1] + ahead[1], cell[2] + ahead[2]];
            let cur = quantization_sample(bpx, bias, noise, &mut rng);
            let chosen_idx = matcher.nearest(cur);
            let chosen_rgb = palette_rgb[chosen_idx];
            let chosen_work = palette_work_color(palette, chosen_idx, opts.color_space);
            out_row[x * 4..x * 4 + 4].copy_from_slice(&[
                chosen_rgb.r,
                chosen_rgb.g,
                chosen_rgb.b,
                alpha,
            ]);

            let err = diffusion_error(bpx, chosen_work, strength);
            let luma =
                (0.2126 * f32::from(px[0]) + 0.7152 * f32::from(px[1]) + 0.0722 * f32::from(px[2]))
                    .round()
                    .clamp(0.0, 255.0) as u8;
            let weights = ostromoukhov_weights(luma);
            ahead = [
                err[0] * weights[0],
                err[1] * weights[0],
                err[2] * weights[0],
            ];
            add_error(&mut rows.next[i.wrapping_add_signed(-dir)], err, weights[1]);
            add_error(&mut rows.next[i], err, weights[2]);
        }
        rows.end_row();
    }

    RgbaImage::from_raw(img.width(), img.height(), dst)
        .unwrap_or_else(|| RgbaImage::new(img.width(), img.height()))
}

fn ordered_dither(
    img: &RgbaImage,
    palette: &Palette,
    opts: &DitherOptions,
    matrix: &[f32],
    size: usize,
) -> RgbaImage {
    let w = img.width() as usize;
    let h = img.height() as usize;
    let src = img.as_raw();
    let mut dst = vec![0u8; w * h * 4];
    let spread = palette.ordered_spread(opts.color_space);
    let strength = opts.strength.clamp(0.0, 1.5);
    let bias = opts.bias.clamp(-0.5, 0.5);
    let noise = opts.noise.clamp(0.0, 0.5);
    let scale = opts.pattern_scale.max(1) as usize;
    let ox = opts.pattern_offset[0] as usize;
    let oy = opts.pattern_offset[1] as usize;
    let colors = palette.colors();
    let matcher = palette.matcher(opts.color_space);

    dst.par_chunks_exact_mut(w * 4)
        .enumerate()
        .for_each(|(y, out_row)| {
            let in_row = &src[y * w * 4..(y + 1) * w * 4];
            let my = ((y / scale) + oy) % size;
            let row_mat = &matrix[my * size..(my + 1) * size];
            let mut rng = SplitMix64::new(
                opts.seed ^ 0xBA7E_2026_1121_0001 ^ (y as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15),
            );

            for x in 0..w {
                let off = x * 4;
                let alpha = in_row[off + 3];
                if alpha == 0 {
                    continue;
                }
                let mx = ((x / scale) + ox) % size;
                let threshold = row_mat[mx] - 0.5;
                let jitter = if noise > 1e-5 {
                    (rng.next_f32() - 0.5) * noise
                } else {
                    0.0
                };
                let work = pixel_to_work(
                    Rgba([in_row[off], in_row[off + 1], in_row[off + 2], alpha]),
                    opts.color_space,
                );
                let sample = [
                    (work[0] + (threshold + jitter) * spread[0] * strength + bias).clamp(0.0, 1.0),
                    (work[1] + (threshold + jitter) * spread[1] * strength + bias).clamp(0.0, 1.0),
                    (work[2] + (threshold + jitter) * spread[2] * strength + bias).clamp(0.0, 1.0),
                ];
                let c = colors[matcher.nearest(sample)];
                out_row[off] = c.r;
                out_row[off + 1] = c.g;
                out_row[off + 2] = c.b;
                out_row[off + 3] = alpha;
            }
        });

    RgbaImage::from_raw(img.width(), img.height(), dst).expect("exact RGBA buffer dimensions")
}

#[derive(Clone, Copy)]
enum StochasticKind {
    Ign,
    Random,
}

fn stochastic_dither(
    img: &RgbaImage,
    palette: &Palette,
    opts: &DitherOptions,
    kind: StochasticKind,
) -> RgbaImage {
    let w = img.width() as usize;
    let h = img.height() as usize;
    let src = img.as_raw();
    let mut dst = vec![0u8; w * h * 4];
    let spread = palette.ordered_spread(opts.color_space);
    let strength = opts.strength.clamp(0.0, 1.5);
    let bias = opts.bias.clamp(-0.5, 0.5);
    let scale = opts.pattern_scale.max(1);
    let seed_offset_x = (opts.seed % 1021) as f32 + opts.pattern_offset[0] as f32;
    let seed_offset_y = ((opts.seed / 1021) % 1031) as f32 + opts.pattern_offset[1] as f32;
    let colors = palette.colors();
    let matcher = palette.matcher(opts.color_space);

    dst.par_chunks_exact_mut(w * 4)
        .enumerate()
        .for_each(|(y, out_row)| {
            let in_row = &src[y * w * 4..(y + 1) * w * 4];
            let gy = (y as u32 / scale) as f32 + seed_offset_y;
            let mut rng = SplitMix64::new(
                opts.seed ^ 0x570C_4A57_1C00_BEEF ^ (y as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9),
            );

            for x in 0..w {
                let off = x * 4;
                let alpha = in_row[off + 3];
                if alpha == 0 {
                    continue;
                }
                let t = match kind {
                    StochasticKind::Ign => {
                        let gx = (x as u32 / scale) as f32 + seed_offset_x;
                        let v =
                            (52.982_918 * (0.067_110_56 * gx + 0.005_837_15 * gy).fract()).fract();
                        v - 0.5
                    }
                    StochasticKind::Random => rng.next_f32() - 0.5,
                };
                let work = pixel_to_work(
                    Rgba([in_row[off], in_row[off + 1], in_row[off + 2], alpha]),
                    opts.color_space,
                );
                let sample = [
                    (work[0] + t * spread[0] * strength + bias).clamp(0.0, 1.0),
                    (work[1] + t * spread[1] * strength + bias).clamp(0.0, 1.0),
                    (work[2] + t * spread[2] * strength + bias).clamp(0.0, 1.0),
                ];
                let c = colors[matcher.nearest(sample)];
                out_row[off] = c.r;
                out_row[off + 1] = c.g;
                out_row[off + 2] = c.b;
                out_row[off + 3] = alpha;
            }
        });

    RgbaImage::from_raw(img.width(), img.height(), dst).expect("exact RGBA buffer dimensions")
}

fn threshold_dither(img: &RgbaImage, palette: &Palette, opts: &DitherOptions) -> RgbaImage {
    let w = img.width() as usize;
    let h = img.height() as usize;
    let src = img.as_raw();
    let mut dst = vec![0u8; w * h * 4];
    let bias = opts.bias.clamp(-0.5, 0.5);
    let colors = palette.colors();
    let matcher = palette.matcher(opts.color_space);

    dst.par_chunks_exact_mut(w * 4)
        .enumerate()
        .for_each(|(y, out_row)| {
            let in_row = &src[y * w * 4..(y + 1) * w * 4];
            for x in 0..w {
                let off = x * 4;
                let alpha = in_row[off + 3];
                if alpha == 0 {
                    continue;
                }
                let work = pixel_to_work(
                    Rgba([in_row[off], in_row[off + 1], in_row[off + 2], alpha]),
                    opts.color_space,
                );
                let sample = [
                    (work[0] + bias).clamp(0.0, 1.0),
                    (work[1] + bias).clamp(0.0, 1.0),
                    (work[2] + bias).clamp(0.0, 1.0),
                ];
                let c = colors[matcher.nearest(sample)];
                out_row[off] = c.r;
                out_row[off + 1] = c.g;
                out_row[off + 2] = c.b;
                out_row[off + 3] = alpha;
            }
        });

    RgbaImage::from_raw(img.width(), img.height(), dst).expect("exact RGBA buffer dimensions")
}

/// Riemersma dithering: error diffusion along a Hilbert space-filling curve using an
/// exponential-decay error history queue (queue length 16, ratio 1/16).
fn riemersma_dither(img: &RgbaImage, palette: &Palette, opts: &DitherOptions) -> RgbaImage {
    const QUEUE_LEN: usize = 16;
    const RATIO: f32 = 1.0 / 16.0;

    let w = img.width();
    let h = img.height();
    let side = w.max(h).max(1).next_power_of_two();
    let mut weights = [0.0f32; QUEUE_LEN];
    let mut weight_sum = 0.0f32;
    for (i, slot) in weights.iter_mut().enumerate() {
        let exp = (i as f32) / ((QUEUE_LEN - 1) as f32);
        let w_i = detmath::powf(RATIO, 1.0 - exp);
        *slot = w_i;
        weight_sum += w_i;
    }
    for slot in &mut weights {
        *slot /= weight_sum;
    }

    // The queue, oldest first, is `queue[oldest..oldest + QUEUE_LEN]`: every entry is stored twice,
    // QUEUE_LEN apart, so the window never wraps.
    let mut queue = [[0.0f32; 3]; 2 * QUEUE_LEN];
    let mut oldest = 0;
    let src = img.as_raw();
    let mut dst = vec![0u8; src.len()];
    let strength = opts.strength.clamp(0.0, 1.5);
    let bias = opts.bias.clamp(-0.5, 0.5);
    let matcher = palette.matcher(opts.color_space);
    let colors = palette.colors();

    hilbert_walk(side, w, h, &mut |x, y| {
        let o = (y as usize * w as usize + x as usize) * 4;
        let px = Rgba([src[o], src[o + 1], src[o + 2], src[o + 3]]);
        if px[3] == 0 {
            return;
        }

        let mut acc = [0.0f32; 3];
        for (q, &wt) in queue[oldest..oldest + QUEUE_LEN].iter().zip(weights.iter()) {
            acc[0] += q[0] * wt;
            acc[1] += q[1] * wt;
            acc[2] += q[2] * wt;
        }

        let work = pixel_to_work(px, opts.color_space);
        let sample = [
            (work[0] + acc[0] * strength + bias).clamp(0.0, 1.0),
            (work[1] + acc[1] * strength + bias).clamp(0.0, 1.0),
            (work[2] + acc[2] * strength + bias).clamp(0.0, 1.0),
        ];
        let chosen_idx = matcher.nearest(sample);
        let chosen_rgb: Rgb = colors[chosen_idx];
        let chosen_work = palette_work_color(palette, chosen_idx, opts.color_space);
        dst[o..o + 4].copy_from_slice(&[chosen_rgb.r, chosen_rgb.g, chosen_rgb.b, px[3]]);

        let err = [
            work[0] - chosen_work[0],
            work[1] - chosen_work[1],
            work[2] - chosen_work[2],
        ];
        queue[oldest] = err;
        queue[oldest + QUEUE_LEN] = err;
        oldest = (oldest + 1) % QUEUE_LEN;
    });

    RgbaImage::from_raw(w, h, dst).expect("exact RGBA buffer dimensions")
}

/// Visit the cells of the `side x side` Hilbert curve (`side` a power of two) that fall inside
/// `width x height`, in curve order, skipping every sub-square that lies wholly outside.
///
/// The curve is the one `hilbert_d2xy` describes. Its top-level quadrants, in order, are
/// lower-left, upper-left, upper-right, lower-right (x right, y up), and the sub-curve in each
/// is the whole curve transposed, unchanged, unchanged, and anti-transposed respectively.
/// `origin` and the signed unit vectors `ax` and `ay` map local curve coordinates to the image:
/// `cell = origin + lx * ax + ly * ay`.
fn hilbert_walk(side: u32, width: u32, height: u32, visit: &mut impl FnMut(u32, u32)) {
    fn walk(
        size: i64,
        origin: [i64; 2],
        ax: [i64; 2],
        ay: [i64; 2],
        bounds: [i64; 2],
        visit: &mut impl FnMut(u32, u32),
    ) {
        // The square this call covers, in image coordinates.
        let far = [
            origin[0] + (size - 1) * (ax[0] + ay[0]),
            origin[1] + (size - 1) * (ax[1] + ay[1]),
        ];
        if origin[0].min(far[0]) >= bounds[0] || origin[1].min(far[1]) >= bounds[1] {
            return;
        }
        if size == 1 {
            visit(origin[0] as u32, origin[1] as u32);
            return;
        }
        let half = size / 2;
        let at = |lx: i64, ly: i64| {
            [
                origin[0] + lx * ax[0] + ly * ay[0],
                origin[1] + lx * ax[1] + ly * ay[1],
            ]
        };
        let neg = |v: [i64; 2]| [-v[0], -v[1]];
        // Lower-left, transposed: local (x, y) -> (y, x).
        walk(half, at(0, 0), ay, ax, bounds, visit);
        // Upper-left and upper-right, unchanged.
        walk(half, at(0, half), ax, ay, bounds, visit);
        walk(half, at(half, half), ax, ay, bounds, visit);
        // Lower-right, anti-transposed: local (x, y) -> (half - 1 - y, half - 1 - x).
        walk(
            half,
            at(2 * half - 1, half - 1),
            neg(ay),
            neg(ax),
            bounds,
            visit,
        );
    }
    walk(
        i64::from(side),
        [0, 0],
        [1, 0],
        [0, 1],
        [i64::from(width), i64::from(height)],
        visit,
    );
}

/// Convert Hilbert curve distance `d` on a `side x side` power-of-two grid into `(x, y)`.
#[cfg(test)]
fn hilbert_d2xy(side: u32, mut d: u64) -> (u32, u32) {
    let mut x = 0u32;
    let mut y = 0u32;
    let mut s = 1u32;
    while s < side {
        let rx = ((d / 2) & 1) as u32;
        let ry = ((d ^ u64::from(rx)) & 1) as u32;
        if ry == 0 {
            if rx == 1 {
                x = s - 1 - x;
                y = s - 1 - y;
            }
            std::mem::swap(&mut x, &mut y);
        }
        x += s * rx;
        y += s * ry;
        d /= 4;
        s <<= 1;
    }
    (x, y)
}

/// Generate a normalized `[0, 1)` Bayer threshold matrix of size `n x n` (`n` power of 2).
fn bayer_matrix(n: usize) -> Vec<f32> {
    let mut mat = vec![0usize; n * n];
    let mut size = 1usize;
    while size < n {
        let next = size * 2;
        let mut next_mat = vec![0usize; next * next];
        for y in 0..size {
            for x in 0..size {
                let base = mat[y * size + x] * 4;
                next_mat[y * next + x] = base;
                next_mat[y * next + (x + size)] = base + 2;
                next_mat[(y + size) * next + x] = base + 3;
                next_mat[(y + size) * next + (x + size)] = base + 1;
            }
        }
        mat = next_mat;
        size = next;
    }
    let denom = (n * n) as f32;
    mat.into_iter().map(|v| (v as f32 + 0.5) / denom).collect()
}

/// 8x8 rotated 45-degree clustered-dot halftone screen normalized to `[0, 1)`.
const HALFTONE_8X8: [f32; 64] = normalize_rank_64([
    24, 10, 12, 26, 35, 47, 49, 37, 8, 0, 2, 14, 45, 59, 61, 51, 22, 6, 4, 16, 43, 57, 63, 53, 30,
    20, 18, 28, 33, 41, 55, 39, 34, 46, 48, 36, 25, 11, 13, 27, 44, 58, 60, 50, 9, 1, 3, 15, 42,
    56, 62, 52, 23, 7, 5, 17, 32, 40, 54, 38, 31, 21, 19, 29,
]);

/// 4x4 tight clustered dot screen.
const CLUSTERED_4X4: [f32; 16] =
    normalize_rank_16([12, 5, 6, 13, 4, 0, 1, 7, 11, 3, 2, 8, 15, 10, 9, 14]);

/// 8x8 diagonal engraving line screen.
const DIAGONAL_8X8: [f32; 64] = normalize_rank_64([
    0, 8, 16, 24, 32, 40, 48, 56, 57, 1, 9, 17, 25, 33, 41, 49, 50, 58, 2, 10, 18, 26, 34, 42, 43,
    51, 59, 3, 11, 19, 27, 35, 36, 44, 52, 60, 4, 12, 20, 28, 29, 37, 45, 53, 61, 5, 13, 21, 22,
    30, 38, 46, 54, 62, 6, 14, 15, 23, 31, 39, 47, 55, 63, 7,
]);

/// 8x8 horizontal line-weight screen (grows symmetrically outward from line center).
const HORIZONTAL_8X8: [f32; 64] = normalize_rank_64([
    56, 58, 60, 62, 63, 61, 59, 57, 40, 42, 44, 46, 47, 45, 43, 41, 24, 26, 28, 30, 31, 29, 27, 25,
    8, 10, 12, 14, 15, 13, 11, 9, 0, 2, 4, 6, 7, 5, 3, 1, 16, 18, 20, 22, 23, 21, 19, 17, 32, 34,
    36, 38, 39, 37, 35, 33, 48, 50, 52, 54, 55, 53, 51, 49,
]);

/// 8x8 vertical etching line-weight screen.
const VERTICAL_8X8: [f32; 64] = normalize_rank_64([
    56, 40, 24, 8, 0, 16, 32, 48, 58, 42, 26, 10, 2, 18, 34, 50, 60, 44, 28, 12, 4, 20, 36, 52, 62,
    46, 30, 14, 6, 22, 38, 54, 63, 47, 31, 15, 7, 23, 39, 55, 61, 45, 29, 13, 5, 21, 37, 53, 59,
    43, 27, 11, 3, 19, 35, 51, 57, 41, 25, 9, 1, 17, 33, 49,
]);

/// 8x8 pen-and-ink crosshatching screen (fills primary diagonal first, then counter-diagonal).
const CROSSHATCH_8X8: [f32; 64] = normalize_rank_64([
    0, 16, 32, 48, 24, 49, 33, 17, 18, 1, 19, 34, 50, 35, 25, 51, 36, 20, 2, 21, 37, 26, 52, 38,
    53, 39, 22, 3, 27, 40, 54, 55, 28, 56, 41, 23, 4, 12, 42, 57, 58, 29, 59, 43, 13, 5, 14, 44,
    45, 60, 30, 61, 46, 15, 6, 11, 10, 47, 62, 31, 63, 9, 8, 7,
]);

const fn normalize_rank_64(ranks: [u8; 64]) -> [f32; 64] {
    let mut out = [0.0f32; 64];
    let mut i = 0;
    while i < 64 {
        out[i] = (ranks[i] as f32 + 0.5) / 64.0;
        i += 1;
    }
    out
}

const fn normalize_rank_16(ranks: [u8; 16]) -> [f32; 16] {
    let mut out = [0.0f32; 16];
    let mut i = 0;
    while i < 16 {
        out[i] = (ranks[i] as f32 + 0.5) / 16.0;
        i += 1;
    }
    out
}

/// Cached 64x64 toroidal Void-and-Cluster blue-noise threshold matrix (Ulichney 1993).
fn blue_noise_64() -> &'static [f32] {
    static BLUE_NOISE: OnceLock<Vec<f32>> = OnceLock::new();
    BLUE_NOISE.get_or_init(|| generate_void_and_cluster(64))
}

/// Fast incremental Ulichney Void-and-Cluster generator producing a genuinely tileable
/// `size x size` blue-noise threshold array in `[0, 1)`.
fn generate_void_and_cluster(size: usize) -> Vec<f32> {
    const RADIUS: i32 = 5;
    const SIGMA: f32 = 1.9;
    let n = size * size;

    // Precompute truncated Gaussian kernel weights.
    let mut offsets: Vec<(i32, i32, f32)> = Vec::new();
    let denom = 2.0 * SIGMA * SIGMA;
    for dy in -RADIUS..=RADIUS {
        for dx in -RADIUS..=RADIUS {
            let d2 = (dx * dx + dy * dy) as f32;
            offsets.push((dx, dy, detmath::exp(-d2 / denom)));
        }
    }

    let add_energy = |energy: &mut [f32], cx: usize, cy: usize, sign: f32| {
        let s = size as i32;
        for &(dx, dy, w) in &offsets {
            let nx = (cx as i32 + dx).rem_euclid(s) as usize;
            let ny = (cy as i32 + dy).rem_euclid(s) as usize;
            energy[ny * size + nx] += sign * w;
        }
    };

    // Initial prototype pattern: ~10% random minority pixels, then relax tightest cluster <-> largest void.
    let init_ones = n / 10;
    let mut rng = SplitMix64::new(0xB10E_4015_E000_0042);
    let mut pattern = vec![false; n];
    let mut energy = vec![0.0f32; n];
    let mut placed = 0usize;
    while placed < init_ones {
        let idx = (rng.next_u64() % n as u64) as usize;
        if !pattern[idx] {
            pattern[idx] = true;
            add_energy(&mut energy, idx % size, idx / size, 1.0);
            placed += 1;
        }
    }

    // Relax initial binary pattern until converged (bounded iterations).
    for _ in 0..256 {
        let mut max_cluster_idx = 0usize;
        let mut max_cluster_val = f32::NEG_INFINITY;
        for (i, (&on, &e)) in pattern.iter().zip(energy.iter()).enumerate() {
            if on && e > max_cluster_val {
                max_cluster_val = e;
                max_cluster_idx = i;
            }
        }
        pattern[max_cluster_idx] = false;
        add_energy(
            &mut energy,
            max_cluster_idx % size,
            max_cluster_idx / size,
            -1.0,
        );

        let mut min_void_idx = 0usize;
        let mut min_void_val = f32::INFINITY;
        for (i, (&on, &e)) in pattern.iter().zip(energy.iter()).enumerate() {
            if !on && e < min_void_val {
                min_void_val = e;
                min_void_idx = i;
            }
        }
        pattern[min_void_idx] = true;
        add_energy(&mut energy, min_void_idx % size, min_void_idx / size, 1.0);

        if min_void_idx == max_cluster_idx {
            break;
        }
    }

    let mut ranks = vec![0u32; n];

    // Phase 1: remove minority pixels down to 0, assigning ranks `init_ones - 1 ..= 0`.
    {
        let mut p1 = pattern.clone();
        let mut e1 = energy.clone();
        for rank in (0..init_ones).rev() {
            let mut best_idx = 0;
            let mut best_val = f32::NEG_INFINITY;
            for (i, (&on, &e)) in p1.iter().zip(e1.iter()).enumerate() {
                if on && e > best_val {
                    best_val = e;
                    best_idx = i;
                }
            }
            p1[best_idx] = false;
            add_energy(&mut e1, best_idx % size, best_idx / size, -1.0);
            ranks[best_idx] = rank as u32;
        }
    }

    // Phase 2: insert pixels from `init_ones .. n/2` into largest voids.
    let half = n / 2;
    for rank in init_ones..half {
        let mut best_idx = 0;
        let mut best_val = f32::INFINITY;
        for (i, (&on, &e)) in pattern.iter().zip(energy.iter()).enumerate() {
            if !on && e < best_val {
                best_val = e;
                best_idx = i;
            }
        }
        pattern[best_idx] = true;
        add_energy(&mut energy, best_idx % size, best_idx / size, 1.0);
        ranks[best_idx] = rank as u32;
    }

    // Phase 3: track zeros (inverse energy) from `n/2 .. n` to keep symmetry.
    let mut inv_energy = vec![0.0f32; n];
    for (i, &on) in pattern.iter().enumerate() {
        if !on {
            add_energy(&mut inv_energy, i % size, i / size, 1.0);
        }
    }
    for rank in half..n {
        let mut best_idx = 0;
        let mut best_val = f32::NEG_INFINITY;
        for (i, (&on, &e)) in pattern.iter().zip(inv_energy.iter()).enumerate() {
            if !on && e > best_val {
                best_val = e;
                best_idx = i;
            }
        }
        pattern[best_idx] = true;
        add_energy(&mut inv_energy, best_idx % size, best_idx / size, -1.0);
        ranks[best_idx] = rank as u32;
    }

    ranks
        .into_iter()
        .map(|r| (r as f32 + 0.5) / (n as f32))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hilbert_walk_visits_in_bounds_cells_in_curve_order() {
        for (w, h) in [
            (1, 1),
            (1, 9),
            (9, 1),
            (2, 2),
            (3, 5),
            (16, 16),
            (17, 3),
            (31, 64),
            (100, 37),
            (64, 65),
        ] {
            let side: u32 = u32::max(w, h).next_power_of_two();
            let expected: Vec<(u32, u32)> = (0..u64::from(side) * u64::from(side))
                .map(|d| hilbert_d2xy(side, d))
                .filter(|&(x, y)| x < w && y < h)
                .collect();
            let mut visited = Vec::new();
            hilbert_walk(side, w, h, &mut |x, y| visited.push((x, y)));
            assert_eq!(visited, expected, "{w}x{h}");
        }
    }

    fn gradient_image(w: u32, h: u32) -> RgbaImage {
        let mut img = RgbaImage::new(w, h);
        for (x, y, px) in img.enumerate_pixels_mut() {
            let v = ((x as f32 / (w - 1).max(1) as f32) * 255.0) as u8;
            let g = ((y as f32 / (h - 1).max(1) as f32) * 255.0) as u8;
            *px = Rgba([v, g, 255 - v, 255]);
        }
        img
    }

    #[test]
    fn every_algorithm_uses_only_palette_colors() {
        let src = gradient_image(32, 24);
        let palette = Palette::by_name("gameboy").unwrap();
        let allowed = palette.colors();

        for &algo in Algorithm::ALL {
            let opts = DitherOptions {
                algorithm: algo,
                ..Default::default()
            };
            let out = dither_image(&src, &palette, &opts);
            assert_eq!(out.dimensions(), src.dimensions(), "algorithm {algo:?}");
            for px in out.pixels() {
                let rgb = Rgb::new(px[0], px[1], px[2]);
                assert!(
                    allowed.contains(&rgb),
                    "algorithm {algo:?} emitted non-palette color {rgb}"
                );
            }
        }
    }

    #[test]
    fn solid_ink_forces_darkest_and_lightest_colors_for_every_algorithm() {
        let src = gradient_image(48, 32);
        let palette = Palette::by_name("gray4").unwrap();
        let (ink, paper) = crate::ink::ink_and_paper(&palette);
        let settings = SolidInk {
            ink_below: Some(0.3),
            paper_above: Some(0.7),
            ink_lines: Some(0.5),
        };
        let masks = crate::ink::InkMasks::compute(&src, &settings);
        assert!(masks.ink.contains(&true) && masks.paper.contains(&true));

        for &algo in Algorithm::ALL {
            let opts = DitherOptions {
                algorithm: algo,
                ink: settings,
                ..Default::default()
            };
            let out = dither_image(&src, &palette, &opts);
            for ((px, &is_ink), &is_paper) in out.pixels().zip(&masks.ink).zip(&masks.paper) {
                let rgb = Rgb::new(px[0], px[1], px[2]);
                if is_ink {
                    assert_eq!(rgb, ink, "algorithm {algo:?}");
                } else if is_paper {
                    assert_eq!(rgb, paper, "algorithm {algo:?}");
                }
            }
            assert_eq!(
                out,
                dither_image(&src, &palette, &opts),
                "algorithm {algo:?}"
            );
        }
    }

    #[test]
    fn neutral_solid_ink_values_leave_every_algorithm_unchanged() {
        let src = gradient_image(48, 32);
        let palette = Palette::by_name("gameboy").unwrap();
        for &algo in Algorithm::ALL {
            let off = DitherOptions {
                algorithm: algo,
                ..Default::default()
            };
            let neutral = DitherOptions {
                ink: SolidInk {
                    ink_below: Some(0.0),
                    paper_above: Some(1.0),
                    ink_lines: Some(0.0),
                },
                ..off.clone()
            };
            assert_eq!(
                dither_image(&src, &palette, &off),
                dither_image(&src, &palette, &neutral),
                "algorithm {algo:?}"
            );
        }
    }

    #[test]
    fn blue_noise_mask_is_uniform_permutation() {
        let mask = blue_noise_64();
        assert_eq!(mask.len(), 64 * 64);
        let mean: f32 = mask.iter().sum::<f32>() / mask.len() as f32;
        assert!((mean - 0.5).abs() < 1e-4, "unexpected mean {mean}");
    }
}
