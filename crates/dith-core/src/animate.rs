//! Highly customizable animated dithering engine:
//! seamless procedural loops, GIF/sequence decoding, easing curves, parameter sweeps,
//! classic palette cycling, Risograph film-boil cadence, exact-palette GIF, APNG, and spritesheets.

use std::collections::HashMap;
use std::f32::consts::PI;
use std::fs::{self, File};
use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{Context, Result};
use image::codecs::gif::GifDecoder;
use image::codecs::png::PngDecoder;
use image::codecs::webp::WebPDecoder;
use image::{AnimationDecoder, Rgba, RgbaImage};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::clock;
use crate::color::Rgb;
use crate::diff::count_changed_pixels;
use crate::dither::dither_image;
use crate::generate::{GenerateOptions, generate_pattern};
use crate::gif_lzw::{self, Disposal};
use crate::output::{
    ColorIndex, ColorUsage, PngLayout, color_histogram, encode_optimized_png_bytes,
    encode_png_bytes, write_output_file,
};
use crate::overlay::{composite_overlay, load_overlay};
use crate::palette::{Palette, extract_adaptive_palette, extract_adaptive_palette_from_images};
use crate::preprocess::{PreparedImage, PreprocessOptions, finalize_image, prepare_image};
use crate::recipe::{DitherRecipe, PaletteSpec, RecipeError};

/// Parse error for `START..END` sweep ranges or `DX,DY` drift vectors.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("invalid range/vector '{input}': {expected}")]
pub struct ParseAnimArgError {
    pub input: String,
    pub expected: &'static str,
}

/// Inclusive floating-point parameter sweep `START..END` (e.g. `-0.4..0.2`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FloatRange {
    pub start: f32,
    pub end: f32,
}

impl FloatRange {
    #[must_use]
    pub fn sample(self, t: f32) -> f32 {
        self.start + (self.end - self.start) * t.clamp(0.0, 1.0)
    }
}

impl FromStr for FloatRange {
    type Err = ParseAnimArgError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let trimmed = s.trim();
        let (a, b) = trimmed
            .split_once("..")
            .or_else(|| trimmed.split_once(':'))
            .ok_or_else(|| ParseAnimArgError {
                input: trimmed.to_string(),
                expected: "expected START..END (e.g. -0.35..0.35 or 0.0..1.2)",
            })?;
        let start = a.trim().parse::<f32>().map_err(|_| ParseAnimArgError {
            input: trimmed.to_string(),
            expected: "expected valid float before '..'",
        })?;
        let end = b.trim().parse::<f32>().map_err(|_| ParseAnimArgError {
            input: trimmed.to_string(),
            expected: "expected valid float after '..'",
        })?;
        Ok(Self { start, end })
    }
}

/// Inclusive integer parameter sweep `START..END` (e.g. `12..1` for de-pixelation reveals).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct U32Range {
    pub start: u32,
    pub end: u32,
}

impl U32Range {
    #[must_use]
    pub fn sample(self, t: f32) -> u32 {
        let t = t.clamp(0.0, 1.0);
        let v = self.start as f32 + (self.end as f32 - self.start as f32) * t;
        v.round().max(1.0) as u32
    }
}

impl FromStr for U32Range {
    type Err = ParseAnimArgError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let trimmed = s.trim();
        let (a, b) = trimmed
            .split_once("..")
            .or_else(|| trimmed.split_once(':'))
            .ok_or_else(|| ParseAnimArgError {
                input: trimmed.to_string(),
                expected: "expected START..END (e.g. 12..1 or 1..8)",
            })?;
        let start = a.trim().parse::<u32>().map_err(|_| ParseAnimArgError {
            input: trimmed.to_string(),
            expected: "expected positive integer before '..'",
        })?;
        let end = b.trim().parse::<u32>().map_err(|_| ParseAnimArgError {
            input: trimmed.to_string(),
            expected: "expected positive integer after '..'",
        })?;
        Ok(Self { start, end })
    }
}

/// Per-frame ordered-screen drift velocity `DX,DY` in screen cells (e.g. `1,0` or `0,-1`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ScreenDrift {
    pub dx: i32,
    pub dy: i32,
}

impl FromStr for ScreenDrift {
    type Err = ParseAnimArgError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let trimmed = s.trim();
        let (a, b) = trimmed.split_once(',').ok_or_else(|| ParseAnimArgError {
            input: trimmed.to_string(),
            expected: "expected DX,DY integers (e.g. 1,0 or 2,-1)",
        })?;
        let dx = a.trim().parse::<i32>().map_err(|_| ParseAnimArgError {
            input: trimmed.to_string(),
            expected: "expected integer DX before ','",
        })?;
        let dy = b.trim().parse::<i32>().map_err(|_| ParseAnimArgError {
            input: trimmed.to_string(),
            expected: "expected integer DY after ','",
        })?;
        Ok(Self { dx, dy })
    }
}

/// Easing curve applied to procedural phase and parameter sweeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "kebab-case")]
pub enum Easing {
    /// Constant steady velocity across the loop.
    #[default]
    Linear,
    /// Smooth sinusoidal acceleration and deceleration.
    EaseInOut,
    /// Accelerating quadratic ease-in.
    EaseIn,
    /// Decelerating quadratic ease-out (ideal for dissolves & focus-in reveals).
    EaseOut,
    /// Classic cubic Hermite S-curve.
    Smoothstep,
    /// Playful elastic bounce landing.
    Bounce,
    /// Quantized stop-motion steps (controlled by `--easing-steps`).
    Steps,
}

impl Easing {
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Linear => "linear",
            Self::EaseInOut => "ease-in-out",
            Self::EaseIn => "ease-in",
            Self::EaseOut => "ease-out",
            Self::Smoothstep => "smoothstep",
            Self::Bounce => "bounce",
            Self::Steps => "steps",
        }
    }

    /// Evaluate the easing curve for `t` in `[0.0, 1.0]`.
    #[must_use]
    pub fn apply(self, t: f32, steps: u32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Self::Linear => t,
            Self::EaseInOut => 0.5 - 0.5 * (t * PI).cos(),
            Self::EaseIn => t * t,
            Self::EaseOut => 1.0 - (1.0 - t) * (1.0 - t),
            Self::Smoothstep => t * t * (3.0 - 2.0 * t),
            Self::Bounce => ease_out_bounce(t),
            Self::Steps => {
                let s = steps.max(2) as f32;
                ((t * s).floor() / (s - 1.0)).clamp(0.0, 1.0)
            }
        }
    }
}

fn ease_out_bounce(t: f32) -> f32 {
    const N1: f32 = 7.5625;
    const D1: f32 = 2.75;
    if t < 1.0 / D1 {
        N1 * t * t
    } else if t < 2.0 / D1 {
        let u = t - 1.5 / D1;
        N1 * u * u + 0.75
    } else if t < 2.5 / D1 {
        let u = t - 2.25 / D1;
        N1 * u * u + 0.9375
    } else {
        let u = t - 2.625 / D1;
        N1 * u * u + 0.984_375
    }
}

/// How parameter sweeps (`--sweep-*`) progress over the animation timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "kebab-case")]
pub enum SweepMode {
    /// Progress cleanly from `start` on frame 0 to `end` on the final frame.
    #[default]
    Linear,
    /// Smoothly breathe `start -> end -> start` over a seamless loop (`sin^2(pi * t)`).
    Pulse,
}

impl SweepMode {
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Linear => "linear",
            Self::Pulse => "pulse",
        }
    }

    #[must_use]
    pub fn shape(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Self::Linear => t,
            Self::Pulse => {
                let s = (t * PI).sin();
                s * s
            }
        }
    }
}

/// Controls how the dither pattern evolves across animation frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "kebab-case")]
pub enum TemporalMode {
    /// Fixed screen & seed across frames. Ordered, blue-noise, ign, and random algorithms leave unchanged
    /// pixels identical; error diffusion still shimmers downstream of anything that moves.
    #[default]
    Locked,
    /// Hand-drawn Risograph / 16mm film boil using golden-ratio shifted screen offsets & subtle jitter.
    Boil,
    /// Energetic animated newsprint/photocopy stipple grain every frame.
    Grain,
    /// Locked, and only pixels near a source change are redrawn: everything else keeps the previous
    /// output frame, so error diffusion cannot ripple a small change across the picture.
    LockedDiff,
}

impl TemporalMode {
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Locked => "locked",
            Self::Boil => "boil",
            Self::Grain => "grain",
            Self::LockedDiff => "locked-diff",
        }
    }

    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Locked => {
                "Fixed screen & seed across frames. With ordered, blue-noise, ign, or random algorithms unchanged pixels stay identical (zero flicker); error-diffusion algorithms still shimmer below/right of anything that moves (Atkinson least)"
            }
            Self::Boil => {
                "Subtle hand-drawn Risograph / 16mm film boil via frame-shifted blue noise & micro-jitter"
            }
            Self::Grain => {
                "Lively animated stipple grain every frame (photocopy / punk zine energy)"
            }
            Self::LockedDiff => {
                "Locked, plus each frame only redraws pixels within --diff-margin of a source change (--diff-threshold); everything else is copied from the previous frame, so error-diffusion animations stay still outside what moves"
            }
        }
    }
}

/// Output container format for animated dithers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "kebab-case")]
pub enum AnimFormat {
    /// Lossless exact-palette looping GIF (direct global color table, tiny file size).
    #[default]
    Gif,
    /// Lossless Animated PNG (indexed palette, delta-encoded frames, alpha, crisp web playback).
    Apng,
    /// Single packed PNG sprite sheet (for CSS `steps()` or game engines).
    Spritesheet,
    /// Directory of numbered lossless PNG frames (`frame_0001.png`, ...).
    Frames,
}

impl AnimFormat {
    /// Infer animation container from output path extension.
    #[must_use]
    pub fn infer_from_path(path: &Path) -> Self {
        match path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("apng") => Self::Apng,
            Some("png") => Self::Spritesheet,
            _ => Self::Gif,
        }
    }

    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Gif => "gif",
            Self::Apng => "apng",
            Self::Spritesheet => "spritesheet",
            Self::Frames => "frames",
        }
    }
}

/// Full animation customization parameters applied during per-frame dithering.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnimDitherOptions {
    pub temporal: TemporalMode,
    /// Boil cadence: update boil pattern every N frames (`1` = every frame, `2` = on twos, `3` = on threes).
    pub boil_rate: u32,
    /// Optional explicit boil/grain jitter intensity (`0.0..=0.5`).
    pub boil_amount: Option<f32>,
    /// Optional per-frame screen cell drift `(dx, dy)` for scrolling halftone/Bayer/scanlines.
    pub screen_drift: Option<ScreenDrift>,
    /// Easing curve for parameter sweeps.
    pub easing: Easing,
    /// Step count when `easing == Easing::Steps`.
    pub easing_steps: u32,
    /// Sweep progression shape (`linear` vs seamless `pulse`).
    pub sweep_mode: SweepMode,
    /// Animate dither threshold bias across frames (ink-wash dissolve!).
    pub sweep_bias: Option<FloatRange>,
    /// Animate contrast across frames.
    pub sweep_contrast: Option<FloatRange>,
    /// Animate brightness across frames (dithered fade-in/fade-out!).
    pub sweep_brightness: Option<FloatRange>,
    /// Animate gamma across frames.
    pub sweep_gamma: Option<FloatRange>,
    /// Animate dither strength across frames (posterize <-> full stipple!).
    pub sweep_strength: Option<FloatRange>,
    /// Animate chunky `pixel_size` across frames (mosaic focus-in reveal!).
    pub sweep_pixel_size: Option<U32Range>,
    /// Classic 90s VGA/Amiga palette color-cycling across frames.
    pub cycle_palette: bool,
    /// Extract an adaptive palette per frame instead of locking one shared global palette.
    pub per_frame_palette: bool,
    /// One overlay file (SVG or PNG) per frame, composited after dithering on top of the recipe's
    /// own overlays, so eyes or lettering can change while the dithered picture stays put.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overlay_frames: Vec<PathBuf>,
    /// `locked-diff`: source luminance change (`0.0..=1.0`) a pixel needs to count as changed
    /// (`0` = any change at all).
    #[serde(default)]
    pub diff_threshold: f32,
    /// `locked-diff`: pixels the changed area grows by before it is redrawn (rounded up to whole
    /// `pixel_size` cells).
    #[serde(default = "default_diff_margin")]
    pub diff_margin: u32,
}

const fn default_diff_margin() -> u32 {
    4
}

impl Default for AnimDitherOptions {
    fn default() -> Self {
        Self {
            temporal: TemporalMode::Locked,
            boil_rate: 1,
            boil_amount: None,
            screen_drift: None,
            easing: Easing::Linear,
            easing_steps: 6,
            sweep_mode: SweepMode::Linear,
            sweep_bias: None,
            sweep_contrast: None,
            sweep_brightness: None,
            sweep_gamma: None,
            sweep_strength: None,
            sweep_pixel_size: None,
            cycle_palette: false,
            per_frame_palette: false,
            overlay_frames: Vec::new(),
            diff_threshold: 0.0,
            diff_margin: default_diff_margin(),
        }
    }
}

/// Parameters controlling procedural phase progression across frames.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProceduralMotion {
    pub frame_count: usize,
    pub fps: u32,
    pub cycles: f32,
    pub phase_offset: f32,
    pub easing: Easing,
    pub easing_steps: u32,
    pub reverse: bool,
}

/// A single RGBA animation frame paired with its display delay in milliseconds.
#[derive(Debug, Clone)]
pub struct AnimFrame {
    pub image: RgbaImage,
    pub delay_ms: u32,
}

/// Result of dithering an entire animation sequence.
#[derive(Debug, Clone)]
pub struct AnimatedOutput {
    pub frames: Vec<AnimFrame>,
    pub palette: Palette,
    pub source_dimensions: [u32; 2],
    pub output_dimensions: [u32; 2],
    pub histogram: Vec<ColorUsage>,
    /// Pixels of each output frame that differ from the frame before it (`0` for the first).
    pub changed_pixels: Vec<u64>,
    pub elapsed_ms: u128,
}

/// Generate seamlessly looping procedural frames in parallel with easing, cycles, and phase offset.
#[must_use]
pub fn generate_procedural_frames(
    base_opts: &GenerateOptions,
    motion: ProceduralMotion,
) -> Vec<AnimFrame> {
    let count = motion.frame_count.max(1);
    let delay_ms = (1000 / motion.fps.clamp(1, 60)).max(16);

    (0..count)
        .into_par_iter()
        .map(|i| {
            let linear_t = i as f32 / count as f32;
            let eased = motion.easing.apply(linear_t, motion.easing_steps);
            let directed = if motion.reverse { 1.0 - eased } else { eased };
            let mut opts = base_opts.clone();
            opts.phase = (motion.phase_offset + directed * motion.cycles).rem_euclid(1.0);
            AnimFrame {
                image: generate_pattern(&opts),
                delay_ms,
            }
        })
        .collect()
}

/// Decode all composited RGBA frames and per-frame delays from an animated GIF file.
///
/// # Errors
/// Returns an error if the file cannot be opened or decoded as a GIF.
pub fn decode_gif_frames(
    path: &Path,
    override_fps: Option<u32>,
    max_frames: Option<usize>,
) -> Result<Vec<AnimFrame>> {
    let file =
        File::open(path).with_context(|| format!("opening animated GIF '{}'", path.display()))?;
    let decoder = GifDecoder::new(BufReader::new(file))
        .with_context(|| format!("decoding GIF header of '{}'", path.display()))?;
    let raw_frames = decoder
        .into_frames()
        .collect_frames()
        .with_context(|| format!("decoding GIF frames of '{}'", path.display()))?;

    Ok(convert_image_frames(raw_frames, override_fps, max_frames))
}

/// Auto-detect and decode multi-frame animations from GIF, APNG (`.apng` / animated `.png`), or
/// animated WebP files. Returns `Ok(None)` if the file is a single static frame or non-animated format.
///
/// # Errors
/// Returns an error if an explicitly animated container cannot be opened or decoded.
pub fn decode_animated_frames(
    path: &Path,
    override_fps: Option<u32>,
    max_frames: Option<usize>,
) -> Result<Option<Vec<AnimFrame>>> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);

    match ext.as_deref() {
        Some("gif") => {
            let frames = decode_gif_frames(path, override_fps, max_frames)?;
            Ok((frames.len() > 1).then_some(frames))
        }
        Some("apng" | "png") => {
            let file =
                File::open(path).with_context(|| format!("opening PNG '{}'", path.display()))?;
            let decoder = PngDecoder::new(BufReader::new(file))
                .with_context(|| format!("decoding PNG header of '{}'", path.display()))?;
            if !decoder.is_apng().unwrap_or(false) {
                return Ok(None);
            }
            let apng = decoder
                .apng()
                .with_context(|| format!("opening APNG stream of '{}'", path.display()))?;
            let raw_frames = apng
                .into_frames()
                .collect_frames()
                .with_context(|| format!("decoding APNG frames of '{}'", path.display()))?;
            let frames = convert_image_frames(raw_frames, override_fps, max_frames);
            Ok((frames.len() > 1).then_some(frames))
        }
        Some("webp") => {
            let file =
                File::open(path).with_context(|| format!("opening WebP '{}'", path.display()))?;
            let decoder = WebPDecoder::new(BufReader::new(file))
                .with_context(|| format!("decoding WebP header of '{}'", path.display()))?;
            if !decoder.has_animation() {
                return Ok(None);
            }
            let raw_frames = decoder
                .into_frames()
                .collect_frames()
                .with_context(|| format!("decoding WebP frames of '{}'", path.display()))?;
            let frames = convert_image_frames(raw_frames, override_fps, max_frames);
            Ok((frames.len() > 1).then_some(frames))
        }
        _ => Ok(None),
    }
}

fn convert_image_frames(
    raw_frames: Vec<image::Frame>,
    override_fps: Option<u32>,
    max_frames: Option<usize>,
) -> Vec<AnimFrame> {
    let limit = max_frames.unwrap_or(usize::MAX).max(1);
    let override_delay = override_fps.map(|f| (1000 / f.clamp(1, 60)).max(16));

    raw_frames
        .into_iter()
        .take(limit)
        .map(|frame| {
            let (numer, denom) = frame.delay().numer_denom_ms();
            let raw_ms = numer.checked_div(denom).unwrap_or(100);
            // Browsers treat 0ms frames as ~100ms; clamp to sensible minimum 20ms.
            let delay_ms = override_delay.unwrap_or(if raw_ms < 20 { 100 } else { raw_ms });
            AnimFrame {
                image: frame.into_buffer(),
                delay_ms,
            }
        })
        .collect()
}

/// Append reversed interior frames so an animation loops back and forth seamlessly. Generic so
/// per-frame side data (delays, overlay choices) can follow the same order as the frames.
pub fn apply_ping_pong<T: Clone>(frames: &mut Vec<T>) {
    if frames.len() <= 2 {
        return;
    }
    let tail: Vec<T> = frames[1..frames.len() - 1].iter().rev().cloned().collect();
    frames.extend(tail);
}

/// Duplicate the first and/or last frame to create crisp hold/dwell pauses at loop boundaries.
/// Each extra copy keeps that frame's own delay, so a hold lasts `count x delay`.
pub fn apply_holds<T: Clone>(frames: &mut Vec<T>, hold_start: usize, hold_end: usize) {
    if frames.is_empty() {
        return;
    }
    if hold_start > 0 {
        let first = frames[0].clone();
        let mut prefix = vec![first; hold_start];
        prefix.append(frames);
        *frames = prefix;
    }
    if hold_end > 0 {
        let last = frames[frames.len() - 1].clone();
        frames.extend(std::iter::repeat_n(last, hold_end));
    }
}

/// Preprocess, resolve palette, and dither all animation frames in parallel with full per-frame
/// sweep, boil cadence, screen drift, and palette cycling controls.
///
/// # Errors
/// Returns a [`RecipeError`] if validation or palette resolution fails.
pub fn dither_animation_frames(
    raw_frames: &[AnimFrame],
    recipe: &DitherRecipe,
    anim_opts: &AnimDitherOptions,
) -> Result<AnimatedOutput, RecipeError> {
    let started = clock::start();
    recipe.validate()?;
    if raw_frames.is_empty() {
        return Err(RecipeError::Validation {
            field: "frames",
            reason: "animation contains zero frames".to_string(),
        });
    }
    if anim_opts.boil_rate == 0 {
        return Err(RecipeError::Validation {
            field: "boil-rate",
            reason: "must be >= 1".to_string(),
        });
    }
    if let Some(amt) = anim_opts.boil_amount
        && !(0.0..=0.5).contains(&amt)
    {
        return Err(RecipeError::Validation {
            field: "boil-amount",
            reason: format!("must be in 0.0..=0.5 (got {amt})"),
        });
    }

    if !(0.0..=1.0).contains(&anim_opts.diff_threshold) {
        return Err(RecipeError::Validation {
            field: "diff-threshold",
            reason: format!("must be in 0.0..=1.0 (got {})", anim_opts.diff_threshold),
        });
    }
    if anim_opts.diff_margin > 256 {
        return Err(RecipeError::Validation {
            field: "diff-margin",
            reason: format!("must be in 0..=256 (got {})", anim_opts.diff_margin),
        });
    }

    let total = raw_frames.len();
    if !anim_opts.overlay_frames.is_empty() && anim_opts.overlay_frames.len() != total {
        return Err(RecipeError::Validation {
            field: "overlay-frames",
            reason: format!(
                "lists {} overlays but the animation has {total} frames",
                anim_opts.overlay_frames.len()
            ),
        });
    }
    let source_dimensions = [raw_frames[0].image.width(), raw_frames[0].image.height()];

    // Step 1: build per-frame preprocess options (applying any tonal/pixel-size sweeps).
    let frame_prep_opts: Vec<PreprocessOptions> = (0..total)
        .map(|frame_idx| {
            let sweep_t = frame_sweep_t(frame_idx, total, anim_opts);
            let mut prep_opts = recipe.preprocess.clone();
            if let Some(r) = anim_opts.sweep_contrast {
                prep_opts.contrast = r.sample(sweep_t).clamp(-1.0, 1.0);
            }
            if let Some(r) = anim_opts.sweep_brightness {
                prep_opts.brightness = r.sample(sweep_t).clamp(-1.0, 1.0);
            }
            if let Some(r) = anim_opts.sweep_gamma {
                prep_opts.gamma = r.sample(sweep_t).clamp(0.1, 5.0);
            }
            if let Some(r) = anim_opts.sweep_pixel_size {
                prep_opts.pixel_size = r.sample(sweep_t).clamp(1, 64);
            }
            prep_opts
        })
        .collect();

    // A frame that repeats the previous frame's pixels and settings (every frame of a boiled
    // still, held frames) shares its prepared image instead of preparing it again.
    let mut unique_frames: Vec<usize> = Vec::new();
    let mut prepared_slot: Vec<usize> = Vec::with_capacity(total);
    for frame_idx in 0..total {
        let repeats_previous = frame_idx > 0
            && frame_prep_opts[frame_idx] == frame_prep_opts[frame_idx - 1]
            && raw_frames[frame_idx].image == raw_frames[frame_idx - 1].image;
        if !repeats_previous {
            unique_frames.push(frame_idx);
        }
        prepared_slot.push(unique_frames.len() - 1);
    }
    let prepared: Vec<PreparedImage> = unique_frames
        .par_iter()
        .map(|&frame_idx| prepare_image(&raw_frames[frame_idx].image, &frame_prep_opts[frame_idx]))
        .collect();
    let prepared_of = |frame_idx: usize| &prepared[prepared_slot[frame_idx]];

    // Step 2: resolve shared global palette (unless --per-frame-palette is explicitly requested).
    let shared_palette = match &recipe.palette {
        PaletteSpec::Adaptive(n) if !anim_opts.per_frame_palette => {
            let working_imgs: Vec<&RgbaImage> = (0..total)
                .map(|frame_idx| &prepared_of(frame_idx).working)
                .collect();
            extract_adaptive_palette_from_images(&working_imgs, *n, recipe.dither.seed)?
        }
        _ => recipe.resolve_palette(&prepared[0].working)?,
    };

    let boil_rate = anim_opts.boil_rate.max(1);

    // Step 3: dither every frame in parallel.
    let dithered: Result<Vec<(RgbaImage, Palette)>, RecipeError> = (0..total)
        .into_par_iter()
        .map(|frame_idx| {
            let prep = prepared_of(frame_idx);
            let sweep_t = frame_sweep_t(frame_idx, total, anim_opts);
            let mut opts = recipe.dither.clone();

            // Apply dither parameter sweeps.
            if let Some(r) = anim_opts.sweep_bias {
                opts.bias = r.sample(sweep_t).clamp(-0.5, 0.5);
            }
            if let Some(r) = anim_opts.sweep_strength {
                opts.strength = r.sample(sweep_t).clamp(0.0, 1.5);
            }

            // Apply temporal boil cadence (e.g. boil_rate=2 animates boil "on twos").
            let boil_tick = (frame_idx as u32) / boil_rate;
            let boil_tick_u64 = u64::from(boil_tick);
            match anim_opts.temporal {
                TemporalMode::Locked | TemporalMode::LockedDiff => {}
                TemporalMode::Boil => {
                    opts.pattern_offset = [boil_tick.wrapping_mul(19), boil_tick.wrapping_mul(31)];
                    opts.seed = opts
                        .seed
                        .wrapping_add(boil_tick_u64.wrapping_mul(0x9E37_79B9_7F4A_7C15));
                    opts.noise = anim_opts
                        .boil_amount
                        .unwrap_or_else(|| opts.noise.max(0.045));
                }
                TemporalMode::Grain => {
                    opts.pattern_offset = [boil_tick.wrapping_mul(29), boil_tick.wrapping_mul(47)];
                    opts.seed = opts
                        .seed
                        .wrapping_add(boil_tick_u64.wrapping_mul(0xBF58_476D_1CE4_E5B9));
                    opts.noise = anim_opts
                        .boil_amount
                        .unwrap_or_else(|| opts.noise.max(0.11));
                }
            }

            // Apply explicit screen drift velocity (e.g. scrolling scanlines or drifting halftones).
            if let Some(drift) = anim_opts.screen_drift {
                let f = frame_idx as i32;
                let dx =
                    (opts.pattern_offset[0] as i32 + f.saturating_mul(drift.dx)).rem_euclid(1024);
                let dy =
                    (opts.pattern_offset[1] as i32 + f.saturating_mul(drift.dy)).rem_euclid(1024);
                opts.pattern_offset = [dx as u32, dy as u32];
            }

            // Per-frame adaptive palette if explicitly requested.
            let frame_palette = if anim_opts.per_frame_palette
                && let PaletteSpec::Adaptive(n) = recipe.palette
            {
                extract_adaptive_palette(&prep.working, n, opts.seed)?
            } else {
                shared_palette.clone()
            };

            let mut dithered = dither_image(&prep.working, &frame_palette, &opts);

            // Classic Amiga/VGA palette color cycling: rotate palette indices smoothly per frame!
            if anim_opts.cycle_palette && frame_palette.len() >= 2 {
                apply_palette_cycle(&mut dithered, &frame_palette, frame_idx, total);
            }

            Ok((dithered, frame_palette))
        })
        .collect();
    let mut dithered = dithered?;

    if anim_opts.temporal == TemporalMode::LockedDiff {
        for frame_idx in 1..total {
            let (done, rest) = dithered.split_at_mut(frame_idx);
            let previous = &done[frame_idx - 1].0;
            let current = &mut rest[0].0;
            let (before, after) = (
                &prepared_of(frame_idx - 1).working,
                &prepared_of(frame_idx).working,
            );
            if previous.dimensions() != current.dimensions()
                || before.dimensions() != after.dimensions()
                || before.dimensions() != current.dimensions()
            {
                continue;
            }
            let cell = frame_prep_opts[frame_idx].pixel_size.max(1);
            let mask = source_change_mask(
                before,
                after,
                anim_opts.diff_threshold,
                anim_opts.diff_margin.div_ceil(cell),
            );
            for ((px, &keep_new), &old) in current.pixels_mut().zip(&mask).zip(previous.pixels()) {
                if !keep_new {
                    *px = old;
                }
            }
        }
    }

    // Step 4: finalize (alpha, pixel-size upscale) and composite overlays as exact ink.
    let (out_w, out_h) = (prepared[0].final_width, prepared[0].final_height);
    let recipe_layers = recipe
        .overlay
        .layers
        .iter()
        .map(|p| load_overlay(p, out_w, out_h))
        .collect::<Result<Vec<_>, _>>()?;
    let mut frame_layers: HashMap<&Path, RgbaImage> = HashMap::new();
    for path in &anim_opts.overlay_frames {
        if !frame_layers.contains_key(path.as_path()) {
            frame_layers.insert(path, load_overlay(path, out_w, out_h)?);
        }
    }
    let dithered_frames: Vec<AnimFrame> = dithered
        .into_par_iter()
        .enumerate()
        .map(|(frame_idx, (img, palette))| {
            let prep_opts = &frame_prep_opts[frame_idx];
            let mut image = finalize_image(img, prepared_of(frame_idx), prep_opts);
            let frame_layer = anim_opts
                .overlay_frames
                .get(frame_idx)
                .map(|p| &frame_layers[p.as_path()]);
            for layer in recipe_layers.iter().chain(frame_layer) {
                composite_overlay(
                    &mut image,
                    layer,
                    &palette,
                    &recipe.overlay,
                    prep_opts.pixel_size,
                );
            }
            AnimFrame {
                image,
                delay_ms: raw_frames[frame_idx].delay_ms,
            }
        })
        .collect();

    let output_dimensions = [
        dithered_frames[0].image.width(),
        dithered_frames[0].image.height(),
    ];
    let histogram = color_histogram(&dithered_frames[0].image);
    let changed_pixels = std::iter::once(0)
        .chain(
            dithered_frames
                .par_windows(2)
                .map(|pair| count_changed_pixels(&pair[0].image, &pair[1].image))
                .collect::<Vec<_>>(),
        )
        .collect();

    Ok(AnimatedOutput {
        frames: dithered_frames,
        palette: shared_palette,
        source_dimensions,
        output_dimensions,
        histogram,
        changed_pixels,
        elapsed_ms: clock::elapsed_ms(started),
    })
}

/// Pixels whose source changed between two prepared frames, grown by `grow` pixels (a square
/// neighbourhood). With `threshold == 0` any RGBA difference counts; otherwise the luminance (or
/// alpha) must move by more than `threshold` (`0.0..=1.0`).
fn source_change_mask(
    before: &RgbaImage,
    after: &RgbaImage,
    threshold: f32,
    grow: u32,
) -> Vec<bool> {
    let luma = |p: &Rgba<u8>| {
        (0.2126 * f32::from(p[0]) + 0.7152 * f32::from(p[1]) + 0.0722 * f32::from(p[2])) / 255.0
    };
    let changed: Vec<bool> = before
        .pixels()
        .zip(after.pixels())
        .map(|(a, b)| {
            if threshold <= 0.0 {
                a != b
            } else {
                (luma(a) - luma(b)).abs() > threshold
                    || f32::from(a[3].abs_diff(b[3])) / 255.0 > threshold
            }
        })
        .collect();
    let (w, h) = (before.width() as usize, before.height() as usize);
    let r = grow as usize;
    if r == 0 {
        return changed;
    }
    // Separable square dilation: rows, then columns.
    let mut rows = vec![false; w * h];
    for y in 0..h {
        let line = &changed[y * w..(y + 1) * w];
        for x in 0..w {
            rows[y * w + x] = line[x.saturating_sub(r)..=(x + r).min(w - 1)].contains(&true);
        }
    }
    let mut grown = vec![false; w * h];
    for x in 0..w {
        for y in 0..h {
            grown[y * w + x] =
                (y.saturating_sub(r)..=(y + r).min(h - 1)).any(|yy| rows[yy * w + x]);
        }
    }
    grown
}

fn frame_sweep_t(frame_idx: usize, total: usize, opts: &AnimDitherOptions) -> f32 {
    let raw = if total <= 1 {
        0.0
    } else {
        frame_idx as f32 / (total - 1) as f32
    };
    let shaped = opts.sweep_mode.shape(raw);
    opts.easing.apply(shaped, opts.easing_steps)
}

fn apply_palette_cycle(img: &mut RgbaImage, palette: &Palette, frame_idx: usize, total: usize) {
    let colors = palette.colors();
    let n = colors.len();
    let shift = ((frame_idx * n) / total.max(1)) % n;
    if shift == 0 {
        return;
    }
    let mut lut: HashMap<[u8; 3], Rgb> = HashMap::with_capacity(n);
    for (i, &c) in colors.iter().enumerate() {
        lut.insert(c.to_array(), colors[(i + shift) % n]);
    }
    for px in img.pixels_mut() {
        if px[3] > 0
            && let Some(&mapped) = lut.get(&[px[0], px[1], px[2]])
        {
            px[0] = mapped.r;
            px[1] = mapped.g;
            px[2] = mapped.b;
        }
    }
}

/// Encode dithered animation frames as a crisp, lossless exact-palette GIF (`loops == 0` -> infinite).
///
/// When the palette has `<= 255` entries, writes the exact palette directly as the GIF Global Color
/// Table (zero `NeuQuant` color drift, exact hex preservation, and ~10x faster encoding).
///
/// Exact-palette animations are also delta-encoded: every frame only stores the bounding box of
/// pixels that differ from what is already on the canvas. Unchanged pixels inside it are written
/// as their color, as a reserved transparent index, or a mix of both, whichever compresses best on
/// the animation's largest frames.
/// Animations with real transparency keep the previous frame wherever nothing has to be erased and
/// clear only the area that does.
///
/// # Errors
/// Returns an error if frames are empty, exceed 65535px, or GIF encoding fails.
pub fn encode_gif(
    frames: &[AnimFrame],
    palette: &Palette,
    loops: u16,
    per_frame_palette: bool,
) -> Result<Vec<u8>> {
    let first = frames
        .first()
        .context("cannot encode animated GIF from empty frame list")?;
    let w = u16::try_from(first.image.width()).context("GIF width exceeds 65535 pixels")?;
    let h = u16::try_from(first.image.height()).context("GIF height exceeds 65535 pixels")?;

    let has_transparency = frames
        .iter()
        .any(|f| f.image.pixels().any(|px| px[3] < 128));
    if per_frame_palette || palette.len() > if has_transparency { 255 } else { 256 } {
        return encode_quantized_gif(frames, w, h, loops);
    }

    let mut color_table: Vec<u8> = palette
        .colors()
        .iter()
        .flat_map(|c| [c.r, c.g, c.b])
        .collect();
    let lookup = ColorIndex::from_palette(
        &palette
            .colors()
            .iter()
            .map(|c| [c.r, c.g, c.b, 255])
            .collect::<Vec<_>>(),
    );
    // Reserved index: real transparency for alpha animations, and "unchanged" in delta frames.
    let reserved: Option<u8> = (palette.len() < 256).then(|| {
        color_table.extend_from_slice(&[0, 0, 0]);
        palette.len() as u8
    });
    let transparent_idx = reserved.filter(|_| has_transparency);

    let index_frames: Vec<Vec<u8>> = frames
        .par_iter()
        .map(|frame| {
            let (pixels, _) = frame.image.as_raw().as_chunks::<4>();
            let mut last: Option<([u8; 4], u8)> = None;
            pixels
                .iter()
                .map(|&px| {
                    if let Some((prev, index)) = last
                        && prev == px
                    {
                        return index;
                    }
                    let index = match transparent_idx {
                        Some(tidx) if px[3] < 128 => tidx,
                        _ => lookup.get([px[0], px[1], px[2], 255]).unwrap_or_else(|| {
                            palette.nearest_index_from_srgb(
                                Rgb::new(px[0], px[1], px[2]).to_srgb_f32(),
                            ) as u8
                        }),
                    };
                    last = Some((px, index));
                    index
                })
                .collect()
        })
        .collect();

    let width = usize::from(w);
    let plan = |i: usize, cleared: Option<GifRegion>| {
        let canvas = GifCanvas {
            prev: i.checked_sub(1).map(|p| index_frames[p].as_slice()),
            cleared,
            width,
            blank: reserved.map_or(256, u16::from),
        };
        let next = index_frames
            .get((i + 1) % index_frames.len())
            .filter(|_| has_transparency)
            .map(Vec::as_slice);
        (canvas, canvas.plan(&index_frames[i], next))
    };
    // Only a cleared area makes a frame depend on how the one before it was stored.
    let plans: Vec<(GifCanvas<'_>, GifRegion, Disposal)> = if has_transparency {
        let mut cleared = None;
        (0..frames.len())
            .map(|i| {
                let (canvas, (region, disposal)) = plan(i, cleared);
                cleared = (disposal == Disposal::Background).then_some(region);
                (canvas, region, disposal)
            })
            .collect()
    } else {
        (0..frames.len())
            .into_par_iter()
            .map(|i| {
                let (canvas, (region, disposal)) = plan(i, None);
                (canvas, region, disposal)
            })
            .collect()
    };

    let transparent = reserved.unwrap_or(0);
    let stored: Vec<(Vec<u8>, Vec<bool>)> = plans
        .par_iter()
        .zip(&index_frames)
        .map(|(&(canvas, region, _), cur)| canvas.frame_pixels(cur, region))
        .collect();

    // Which way to fill unchanged pixels hardly varies within one animation, so every rule is
    // tried on the two largest frames that have the choice and the winner is used for the rest.
    let mut probes: Vec<usize> = (0..stored.len())
        .filter(|&i| !stored[i].1.is_empty())
        .collect();
    probes.sort_by_key(|&i| std::cmp::Reverse(stored[i].0.len()));
    probes.truncate(2);
    let probe_results: Vec<(usize, UnchangedFill, (u8, Vec<u8>))> = probes
        .iter()
        .flat_map(|&i| UnchangedFill::ALL.map(|fill| (i, fill)))
        .collect::<Vec<_>>()
        .into_par_iter()
        .map(|(i, fill)| {
            let (pixels, optional) = &stored[i];
            (i, fill, fill.compress(pixels, optional, transparent))
        })
        .collect();
    let chosen = UnchangedFill::ALL
        .into_iter()
        .min_by_key(|&fill| {
            probe_results
                .iter()
                .filter(|r| r.1 == fill)
                .map(|r| r.2.1.len())
                .sum::<usize>()
        })
        .unwrap_or(UnchangedFill::Color);

    let gif_frames: Vec<gif_lzw::Frame> = plans
        .par_iter()
        .zip(&stored)
        .zip(frames)
        .enumerate()
        .map(
            |(i, ((&(_, region, disposal), (pixels, optional)), frame))| {
                let (min_code_size, lzw) = probe_results
                    .iter()
                    .filter(|r| r.0 == i)
                    .min_by_key(|r| r.2.1.len())
                    .map_or_else(
                        || chosen.compress(pixels, optional, transparent),
                        |r| r.2.clone(),
                    );
                // Regions lie inside the u16 canvas, so these conversions cannot truncate.
                gif_lzw::Frame {
                    left: region.x as u16,
                    top: region.y as u16,
                    width: region.w as u16,
                    height: region.h as u16,
                    delay: gif_delay_centis(frame.delay_ms),
                    disposal,
                    transparent: reserved.filter(|_| has_transparency || !optional.is_empty()),
                    min_code_size,
                    lzw,
                }
            },
        )
        .collect();

    Ok(gif_lzw::write_gif(
        w,
        h,
        &color_table,
        Some(loops),
        &gif_frames,
    ))
}

/// How a delta frame stores pixels that already show the right color, which it may write either
/// as that color or as the transparent index. Boiling dither compresses best with its colors,
/// static backgrounds as transparent runs, and some animations with a mix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnchangedFill {
    /// Every pixel keeps its color.
    Color,
    /// Every unchanged pixel is transparent.
    Transparent,
    /// Runs of at least [`GIF_TRANSPARENT_RUN`] unchanged pixels are transparent; in shorter runs
    /// the LZW encoder picks color or transparent per pixel, whichever continues its match further.
    Mixed,
}

/// Unchanged pixels in a run of at least this many are transparent under [`UnchangedFill::Mixed`].
const GIF_TRANSPARENT_RUN: usize = 16;

impl UnchangedFill {
    const ALL: [Self; 3] = [Self::Color, Self::Transparent, Self::Mixed];

    /// LZW-compress one frame's `pixels`, where `optional` marks those that may be stored as
    /// `transparent`, returning the minimum code size and code stream.
    fn compress(self, pixels: &[u8], optional: &[bool], transparent: u8) -> (u8, Vec<u8>) {
        if self == Self::Color {
            return gif_lzw::compress(&gif_lzw::Indices {
                pixels,
                optional: &[],
                transparent,
            });
        }
        let mut filled = pixels.to_vec();
        let mut still_optional = optional.to_vec();
        let mut start = 0;
        for run in optional.chunk_by(|a, b| a == b) {
            let end = start + run.len();
            if run[0] && (self == Self::Transparent || run.len() >= GIF_TRANSPARENT_RUN) {
                filled[start..end].fill(transparent);
                still_optional[start..end].fill(false);
            }
            start = end;
        }
        if !still_optional.contains(&true) {
            still_optional.clear();
        }
        gif_lzw::compress(&gif_lzw::Indices {
            pixels: &filled,
            optional: &still_optional,
            transparent,
        })
    }
}

/// GIF frame delay in centiseconds for a delay in milliseconds: GIF stores 10 ms steps, and
/// browsers stretch anything under 20 ms, so delays round to the nearest step with a 20 ms floor.
#[must_use]
pub fn gif_delay_centis(delay_ms: u32) -> u16 {
    ((delay_ms + 5) / 10).clamp(2, u32::from(u16::MAX)) as u16
}

/// Encode frames whose colors do not fit one exact color table: each frame is quantized to its
/// own palette and stored whole.
fn encode_quantized_gif(frames: &[AnimFrame], w: u16, h: u16, loops: u16) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut encoder =
            gif::Encoder::new(&mut out, w, h, &[]).context("initializing GIF encoder")?;
        let repeat = if loops == 0 {
            gif::Repeat::Infinite
        } else {
            gif::Repeat::Finite(loops)
        };
        encoder
            .set_repeat(repeat)
            .context("setting GIF loop count")?;
        for frame in frames {
            let mut rgba = frame.image.as_raw().clone();
            let mut gf = gif::Frame::from_rgba_speed(w, h, &mut rgba, 10);
            gf.delay = gif_delay_centis(frame.delay_ms);
            gf.dispose = gif::DisposalMethod::Background;
            encoder.write_frame(&gf).context("writing GIF frame")?;
        }
    }
    Ok(out)
}

/// Rectangle of the canvas that one GIF frame covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GifRegion {
    x: usize,
    y: usize,
    w: usize,
    h: usize,
}

impl GifRegion {
    fn contains(self, x: usize, y: usize) -> bool {
        (self.x..self.x + self.w).contains(&x) && (self.y..self.y + self.h).contains(&y)
    }
}

/// What a decoder's canvas shows just before a frame is drawn: the previous frame's palette
/// indices, minus the area that frame cleared when it was disposed.
#[derive(Debug, Clone, Copy)]
struct GifCanvas<'a> {
    prev: Option<&'a [u8]>,
    cleared: Option<GifRegion>,
    width: usize,
    /// Value standing for a transparent canvas pixel: the reserved index, or 256 without one.
    blank: u16,
}

impl GifCanvas<'_> {
    fn at(&self, x: usize, y: usize) -> u16 {
        match self.prev {
            Some(prev) if !self.cleared.is_some_and(|c| c.contains(x, y)) => {
                u16::from(prev[y * self.width + x])
            }
            _ => self.blank,
        }
    }

    /// Choose the region frame `cur` has to store and how to dispose of it. The first frame covers
    /// the whole canvas, later ones every pixel that differs from the canvas. When `next` turns
    /// pixels transparent that are opaque in `cur`, the region also covers those and is cleared
    /// after display, because a kept frame can only be painted over, never erased. The last frame's
    /// `next` is the first, so the loop restarts on an exact canvas.
    fn plan(&self, cur: &[u8], next: Option<&[u8]>) -> (GifRegion, Disposal) {
        let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0usize, 0usize);
        let mut must_clear = false;
        for (y, row) in cur.chunks_exact(self.width).enumerate() {
            let next_row = next.map(|n| &n[y * self.width..(y + 1) * self.width]);
            let erased = |x: usize| {
                next_row.is_some_and(|n| u16::from(n[x]) == self.blank)
                    && u16::from(row[x]) != self.blank
            };
            let stored =
                |x: usize| self.prev.is_none() || self.at(x, y) != u16::from(row[x]) || erased(x);
            if let Some(first) = (0..self.width).position(stored) {
                let last = (0..self.width).rposition(stored).unwrap_or(first);
                x0 = x0.min(first);
                x1 = x1.max(last);
                y0 = y0.min(y);
                y1 = y1.max(y);
                must_clear = must_clear || (first..=last).any(erased);
            }
        }
        if x0 == usize::MAX {
            // Nothing to draw or erase: a single pixel that leaves the canvas untouched.
            let region = GifRegion {
                x: 0,
                y: 0,
                w: 1,
                h: 1,
            };
            return (region, Disposal::Keep);
        }
        let region = GifRegion {
            x: x0,
            y: y0,
            w: x1 - x0 + 1,
            h: y1 - y0 + 1,
        };
        let disposal = if must_clear {
            Disposal::Background
        } else {
            Disposal::Keep
        };
        (region, disposal)
    }

    /// Palette indices of `cur` inside `region`, with a flag per pixel that is set where the
    /// canvas already shows that color, so the pixel may be stored as transparent instead. The
    /// flags are empty when no pixel has that choice.
    fn frame_pixels(&self, cur: &[u8], region: GifRegion) -> (Vec<u8>, Vec<bool>) {
        let mut pixels = Vec::with_capacity(region.w * region.h);
        let mut optional = Vec::with_capacity(region.w * region.h);
        for y in region.y..region.y + region.h {
            for x in region.x..region.x + region.w {
                let index = cur[y * self.width + x];
                pixels.push(index);
                optional.push(
                    self.blank < 256
                        && u16::from(index) != self.blank
                        && self.at(x, y) == u16::from(index),
                );
            }
        }
        if !optional.contains(&true) {
            optional.clear();
        }
        (pixels, optional)
    }
}

/// Encode dithered animation frames as a lossless Animated PNG (`loops == 0` -> infinite).
///
/// All frames share one compact [`PngLayout`] (an indexed palette for dithered output), and every
/// frame after the first only stores the bounding box of pixels that changed since the previous one.
/// When every changed pixel in that box is opaque, the frame may instead be blended over the canvas
/// with its unchanged pixels fully transparent, so static areas inside the box compress to almost
/// nothing; see [`blend_unchanged_over`] for how that is chosen.
/// Every frame is compressed with the crate's DEFLATE encoder; with `optimize`, frames are also
/// re-filtered and tried with Zopfli for the smallest file.
///
/// # Errors
/// Returns an error if frames are empty or PNG encoding fails.
pub fn encode_apng(frames: &[AnimFrame], loops: u32, optimize: bool) -> Result<Vec<u8>> {
    let first = frames
        .first()
        .context("cannot encode APNG from empty frame list")?;
    let w = first.image.width();
    let h = first.image.height();
    let num_frames = u32::try_from(frames.len()).context("frame count exceeds u32")?;
    if let Some((i, frame)) = frames
        .iter()
        .enumerate()
        .find(|(_, f)| f.image.dimensions() != (w, h))
    {
        let (fw, fh) = frame.image.dimensions();
        anyhow::bail!(
            "cannot encode APNG: frame {} is {fw}x{fh} but frame 1 is {w}x{h}; all frames must share one size",
            i + 1
        );
    }
    let mut stored: Vec<ApngFrame> = (0..frames.len())
        .into_par_iter()
        .map(|i| {
            let cur = &frames[i].image;
            let (x, y, rw, rh) = i
                .checked_sub(1)
                .map_or((0, 0, w, h), |p| changed_region(&frames[p].image, cur));
            ApngFrame {
                x,
                y,
                pixels: image::imageops::crop_imm(cur, x, y, rw, rh).to_image(),
                blend: png::BlendOp::Source,
            }
        })
        .collect();
    let layout = blend_unchanged_over(
        PngLayout::for_images(frames.iter().map(|f| &f.image)),
        frames,
        &mut stored,
    );

    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, w, h);
        layout.configure(&mut encoder, png::Compression::NoCompression);
        encoder
            .set_animated(num_frames, loops)
            .context("configuring APNG animation header")?;
        encoder
            .set_dispose_op(png::DisposeOp::None)
            .context("setting APNG dispose op")?;

        let mut writer = encoder.write_header().context("writing APNG header")?;
        for (frame, part) in frames.iter().zip(&stored) {
            let delay_ms =
                u16::try_from(frame.delay_ms.clamp(10, u32::from(u16::MAX))).unwrap_or(u16::MAX);
            writer
                .set_frame_delay(delay_ms, 1000)
                .context("setting APNG frame delay")?;
            writer
                .reset_frame_position()
                .and_then(|()| {
                    writer.set_frame_dimension(part.pixels.width(), part.pixels.height())
                })
                .and_then(|()| writer.set_frame_position(part.x, part.y))
                .and_then(|()| writer.set_blend_op(part.blend))
                .context("setting APNG frame region")?;
            writer
                .write_image_data(&layout.pixel_data(&part.pixels))
                .context("writing APNG frame pixels")?;
        }
        writer.finish().context("finalizing APNG stream")?;
    }
    crate::png_optimize::recompress_with(
        &out,
        if optimize {
            crate::png_optimize::Search::Max
        } else {
            crate::png_optimize::Search::Fast
        },
    )
}

/// Over blending is used only where its trial size is below this many 1024ths of the Source
/// frame's (and a new transparent palette entry only when the whole animation's trial total is),
/// since the fast trial compression misjudges small differences: at 1024, one boiling 8-color
/// test animation chose Over on its trials and grew 5.5% at full compression.
const APNG_OVER_MARGIN: usize = 819;

/// One stored APNG frame: the pixels of a canvas region and how they are blended onto it.
#[derive(Debug)]
struct ApngFrame {
    x: u32,
    y: u32,
    pixels: RgbaImage,
    blend: png::BlendOp,
}

/// Switch delta frames to Over blending with their unchanged pixels fully transparent wherever a
/// fast trial compression says that is clearly smaller, and return the layout to encode with
/// (`plain` unless a transparent palette entry had to be added). Over is exact only where the new
/// pixel is opaque or the canvas already shows it, so frames that change a pixel to a translucent
/// one stay Source. Frames with too few unchanged pixels to reach the margin skip the trials, and
/// when Over wins on neither of the two largest delta frames no other frame is tried. An
/// indexed palette without a transparent entry gets one, which can raise the bit depth of every
/// frame, so it is only kept when the trial total of the whole animation passes the same margin.
fn blend_unchanged_over(
    plain: PngLayout,
    frames: &[AnimFrame],
    stored: &mut [ApngFrame],
) -> PngLayout {
    let PngLayout::Indexed(palette) = &plain else {
        return plain;
    };
    let existing = palette.iter().copied().find(|c| c[3] == 0);
    if existing.is_none() && palette.len() == 256 {
        return plain;
    }
    let clear = existing.unwrap_or([0; 4]);
    let extended = existing.is_none().then(|| {
        let mut colors = palette.clone();
        colors.insert(colors.iter().take_while(|c| c[3] < 255).count(), clear);
        PngLayout::Indexed(colors)
    });
    let layout = extended.as_ref().unwrap_or(&plain);

    let trial = |i: usize| {
        let part = &stored[i];
        let Some(canvas) = i.checked_sub(1).map(|p| &frames[p].image) else {
            return ApngTrial::default();
        };
        let mut over = part.pixels.clone();
        let row_bytes = over.width() as usize * 4;
        let canvas_stride = canvas.width() as usize * 4;
        let mut unchanged = 0;
        for (y, row) in over.chunks_exact_mut(row_bytes).enumerate() {
            let start = (part.y as usize + y) * canvas_stride + part.x as usize * 4;
            let canvas_row = &canvas.as_raw()[start..start + row_bytes];
            let (shown, _) = canvas_row.as_chunks::<4>();
            for (px, shown) in row.as_chunks_mut::<4>().0.iter_mut().zip(shown) {
                if px == shown {
                    *px = clear;
                    unchanged += 1;
                } else if px[3] != 255 {
                    return ApngTrial::default();
                }
            }
        }
        if unchanged * 4 * 1024 < over.len() * (1024 - APNG_OVER_MARGIN) {
            return ApngTrial::default();
        }
        let source = trial_size(layout, &part.pixels);
        let size = trial_size(layout, &over);
        ApngTrial {
            source: Some(source),
            over: (size * 1024 < source * APNG_OVER_MARGIN).then_some((size, over)),
        }
    };
    let mut probes: Vec<usize> = (1..stored.len()).collect();
    probes.sort_by_key(|&i| std::cmp::Reverse(stored[i].pixels.len()));
    probes.truncate(2);
    if probes.par_iter().all(|&i| trial(i).over.is_none()) {
        return plain;
    }
    let trials: Vec<ApngTrial> = (0..stored.len()).into_par_iter().map(&trial).collect();
    if trials.iter().all(|t| t.over.is_none()) {
        return plain;
    }
    if extended.is_some() {
        let (total, plain_total) = stored
            .par_iter()
            .zip(&trials)
            .map(|(part, t)| {
                let stored_size = t.over.as_ref().map_or_else(
                    || t.source.unwrap_or_else(|| trial_size(layout, &part.pixels)),
                    |o| o.0,
                );
                (stored_size, trial_size(&plain, &part.pixels))
            })
            .reduce(|| (0, 0), |a, b| (a.0 + b.0, a.1 + b.1));
        if total * 1024 >= plain_total * APNG_OVER_MARGIN {
            return plain;
        }
    }
    for (part, trial) in stored.iter_mut().zip(trials) {
        if let Some((_, pixels)) = trial.over {
            part.pixels = pixels;
            part.blend = png::BlendOp::Over;
        }
    }
    extended.unwrap_or(plain)
}

/// Trial sizes of one delta APNG frame whose unchanged pixels could be blended over the canvas:
/// stored as it is, and blended with its unchanged pixels transparent when that is clearly smaller.
#[derive(Debug, Default)]
struct ApngTrial {
    source: Option<usize>,
    over: Option<(usize, RgbaImage)>,
}

/// Size of `img` in `layout` after a fast DEFLATE pass, as a cheap stand-in for its final size.
fn trial_size(layout: &PngLayout, img: &RgbaImage) -> usize {
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder
        .write_all(&layout.pixel_data(img))
        .and_then(|()| encoder.finish())
        .map_or(usize::MAX, |out| out.len())
}

/// Bounding box `(x, y, width, height)` of pixels that differ between two equally sized frames
/// (a single pixel at the origin when they are identical, since APNG frames cannot be empty).
fn changed_region(prev: &RgbaImage, cur: &RgbaImage) -> (u32, u32, u32, u32) {
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
    for (x, y, px) in cur.enumerate_pixels() {
        if prev.get_pixel(x, y) != px {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
    }
    if x0 == u32::MAX {
        (0, 0, 1, 1)
    } else {
        (x0, y0, x1 - x0 + 1, y1 - y0 + 1)
    }
}

/// Pack animation frames into a crisp PNG sprite sheet `(image, columns, rows)`.
#[must_use]
pub fn compose_spritesheet(frames: &[AnimFrame], columns: Option<u32>) -> (RgbaImage, u32, u32) {
    if frames.is_empty() {
        return (RgbaImage::from_pixel(1, 1, Rgba([0, 0, 0, 0])), 1, 1);
    }
    let count = frames.len() as u32;
    let cols = columns.unwrap_or(count).clamp(1, count);
    let rows = count.div_ceil(cols);
    let fw = frames[0].image.width();
    let fh = frames[0].image.height();

    let mut sheet = RgbaImage::from_pixel(fw * cols, fh * rows, Rgba([0, 0, 0, 0]));
    for (idx, frame) in frames.iter().enumerate() {
        let col = (idx as u32) % cols;
        let row = (idx as u32) / cols;
        let ox = col * fw;
        let oy = row * fh;
        for (x, y, &px) in frame.image.enumerate_pixels() {
            if ox + x < sheet.width() && oy + y < sheet.height() {
                sheet.put_pixel(ox + x, oy + y, px);
            }
        }
    }
    (sheet, cols, rows)
}

/// Write numbered PNG frames (`frame_0001.png`, ...) into `dir` and return their written paths.
/// With `optimize`, each frame is encoded with [`encode_optimized_png_bytes`].
///
/// # Errors
/// Returns an error if directory creation, PNG encoding, or file writing fails.
pub fn write_frames_directory(
    frames: &[AnimFrame],
    dir: &Path,
    optimize: bool,
) -> Result<Vec<PathBuf>> {
    fs::create_dir_all(dir)
        .with_context(|| format!("creating frames directory '{}'", dir.display()))?;

    let encoded: Result<Vec<(PathBuf, Vec<u8>)>> = frames
        .par_iter()
        .enumerate()
        .map(|(i, frame)| {
            let path = dir.join(format!("frame_{:04}.png", i + 1));
            let bytes = if optimize {
                encode_optimized_png_bytes(&frame.image)?
            } else {
                encode_png_bytes(&frame.image)?
            };
            Ok((path, bytes))
        })
        .collect();
    let encoded = encoded?;

    let mut paths = Vec::with_capacity(encoded.len());
    for (path, bytes) in encoded {
        write_output_file(&path, &bytes)?;
        paths.push(path);
    }
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generate::Pattern;

    #[test]
    fn ranges_and_easings_parse_and_evaluate_cleanly() {
        let fr: FloatRange = "-0.4..0.2".parse().unwrap();
        assert!((fr.sample(0.0) - (-0.4)).abs() < 1e-5);
        assert!((fr.sample(1.0) - 0.2).abs() < 1e-5);

        let ur: U32Range = "12..2".parse().unwrap();
        assert_eq!(ur.sample(0.0), 12);
        assert_eq!(ur.sample(1.0), 2);

        let drift: ScreenDrift = "2,-1".parse().unwrap();
        assert_eq!((drift.dx, drift.dy), (2, -1));

        for easing in [
            Easing::Linear,
            Easing::EaseInOut,
            Easing::EaseIn,
            Easing::EaseOut,
            Easing::Smoothstep,
            Easing::Bounce,
            Easing::Steps,
        ] {
            assert!(easing.apply(0.0, 5) <= 1e-4, "easing {easing:?} at 0");
            assert!(
                (easing.apply(1.0, 5) - 1.0).abs() <= 1e-4,
                "easing {easing:?} at 1"
            );
        }
    }

    #[test]
    fn procedural_animation_with_sweeps_and_cycling_encodes_gif_and_apng() {
        let gen_opts = GenerateOptions {
            pattern: Pattern::Sphere,
            width: 48,
            height: 36,
            ..Default::default()
        };
        let raw = generate_procedural_frames(
            &gen_opts,
            ProceduralMotion {
                frame_count: 6,
                fps: 12,
                cycles: 1.0,
                phase_offset: 0.0,
                easing: Easing::EaseInOut,
                easing_steps: 4,
                reverse: false,
            },
        );
        assert_eq!(raw.len(), 6);

        let recipe = DitherRecipe::from_preset("gameboy").unwrap();
        let anim_opts = AnimDitherOptions {
            temporal: TemporalMode::Boil,
            boil_rate: 2,
            boil_amount: Some(0.06),
            screen_drift: Some(ScreenDrift { dx: 1, dy: 0 }),
            sweep_bias: Some(FloatRange {
                start: -0.2,
                end: 0.2,
            }),
            sweep_pixel_size: Some(U32Range { start: 4, end: 1 }),
            cycle_palette: true,
            ..Default::default()
        };
        let anim = dither_animation_frames(&raw, &recipe, &anim_opts).unwrap();
        assert_eq!(anim.frames.len(), 6);
        assert_eq!(anim.output_dimensions, [48, 36]);

        let gif_bytes = encode_gif(&anim.frames, &anim.palette, 0, false).unwrap();
        assert!(gif_bytes.starts_with(b"GIF89a"));

        let apng_bytes = encode_apng(&anim.frames, 0, false).unwrap();
        assert!(apng_bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert!(
            apng_bytes.windows(4).any(|w| w == b"acTL"),
            "expected APNG acTL animation control chunk"
        );

        let (sheet, cols, rows) = compose_spritesheet(&anim.frames, Some(3));
        assert_eq!((cols, rows), (3, 2));
        assert_eq!(sheet.dimensions(), (48 * 3, 36 * 2));

        // Verify decode_animated_frames decodes both GIF and APNG cleanly.
        let dir = tempfile::tempdir().unwrap();
        let gif_path = dir.path().join("loop.gif");
        let apng_path = dir.path().join("loop.apng");
        fs::write(&gif_path, &gif_bytes).unwrap();
        fs::write(&apng_path, &apng_bytes).unwrap();
        let dec_gif = decode_animated_frames(&gif_path, None, None)
            .unwrap()
            .expect("gif has multiple frames");
        let dec_apng = decode_animated_frames(&apng_path, None, None)
            .unwrap()
            .expect("apng has multiple frames");
        assert_eq!(dec_gif.len(), 6);
        assert_eq!(dec_apng.len(), 6);
    }

    fn assert_gif_roundtrips_exactly(frames: &[AnimFrame], palette: &Palette) -> usize {
        let bytes = encode_gif(frames, palette, 0, false).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("roundtrip.gif");
        fs::write(&path, &bytes).unwrap();
        let decoded = decode_gif_frames(&path, None, None).unwrap();
        assert_eq!(decoded.len(), frames.len());
        for (i, (dec, src)) in decoded.iter().zip(frames).enumerate() {
            assert_eq!(
                dec.image.dimensions(),
                src.image.dimensions(),
                "frame {i} size"
            );
            for (d, s) in dec.image.pixels().zip(src.image.pixels()) {
                assert_eq!(
                    &d.0[..3],
                    &s.0[..3],
                    "frame {i} differs after delta encoding"
                );
                assert_eq!(d.0[3], 255, "frame {i} has unexpected transparency");
            }
        }
        bytes.len()
    }

    #[test]
    fn delta_encoded_gif_decodes_to_identical_frames() {
        let gen_opts = GenerateOptions {
            pattern: Pattern::Sphere,
            width: 64,
            height: 48,
            ..Default::default()
        };
        let mut raw = generate_procedural_frames(
            &gen_opts,
            ProceduralMotion {
                frame_count: 8,
                fps: 10,
                cycles: 1.0,
                phase_offset: 0.0,
                easing: Easing::Linear,
                easing_steps: 4,
                reverse: false,
            },
        );
        // Holds produce identical consecutive frames, exercising the empty-diff path.
        apply_holds(&mut raw, 0, 2);

        for (preset, temporal) in [
            ("mac-classic", TemporalMode::Locked),
            ("film-stipple", TemporalMode::Locked),
            ("gameboy", TemporalMode::Boil),
            ("pico8", TemporalMode::Grain),
        ] {
            let recipe = DitherRecipe::from_preset(preset).unwrap();
            let anim_opts = AnimDitherOptions {
                temporal,
                ..Default::default()
            };
            let anim = dither_animation_frames(&raw, &recipe, &anim_opts).unwrap();
            assert_gif_roundtrips_exactly(&anim.frames, &anim.palette);
            assert_apng_roundtrips_exactly(&anim.frames);
        }
    }

    fn assert_apng_roundtrips_exactly(frames: &[AnimFrame]) {
        for optimize in [false, true] {
            let bytes = encode_apng(frames, 0, optimize).unwrap();
            let decoded: Vec<RgbaImage> = PngDecoder::new(std::io::Cursor::new(bytes))
                .unwrap()
                .apng()
                .unwrap()
                .into_frames()
                .map(|f| f.unwrap().into_buffer())
                .collect();
            assert_eq!(decoded.len(), frames.len());
            for (i, (dec, frame)) in decoded.iter().zip(frames).enumerate() {
                assert_eq!(
                    dec, &frame.image,
                    "APNG frame {i} differs after decode (optimize: {optimize})"
                );
            }
        }
    }

    #[test]
    fn solid_ink_masks_are_identical_on_every_frame_of_a_still() {
        let still = RgbaImage::from_fn(80, 60, |x, y| {
            let v = if (36..40).contains(&x) {
                0
            } else if y < 12 {
                20
            } else {
                (90 + x * 2).min(255) as u8
            };
            Rgba([v, v, v, 255])
        });
        let raw = vec![
            AnimFrame {
                image: still.clone(),
                delay_ms: 80,
            };
            6
        ];
        let recipe = DitherRecipe::from_preset("manga").unwrap();
        let working = prepare_image(&still, &recipe.preprocess).working;
        let masks = crate::ink::InkMasks::compute(&working, &recipe.dither.ink);
        assert!(masks.ink.contains(&true) && masks.paper.contains(&true));

        for temporal in [TemporalMode::Locked, TemporalMode::Boil] {
            let anim_opts = AnimDitherOptions {
                temporal,
                ..Default::default()
            };
            let anim = dither_animation_frames(&raw, &recipe, &anim_opts).unwrap();
            for (i, frame) in anim.frames.iter().enumerate() {
                for ((px, &is_ink), &is_paper) in
                    frame.image.pixels().zip(&masks.ink).zip(&masks.paper)
                {
                    if is_ink {
                        assert_eq!(px.0, [0, 0, 0, 255], "{temporal:?} frame {i}");
                    } else if is_paper {
                        assert_eq!(px.0, [255, 255, 255, 255], "{temporal:?} frame {i}");
                    }
                }
                if temporal == TemporalMode::Locked {
                    assert_eq!(frame.image, anim.frames[0].image, "locked frame {i}");
                }
            }
        }
    }

    #[test]
    fn source_change_mask_honours_threshold_and_margin() {
        let before = RgbaImage::from_pixel(9, 9, Rgba([100, 100, 100, 255]));
        let mut after = before.clone();
        after.put_pixel(4, 4, Rgba([110, 100, 100, 255]));
        let count = |m: &[bool]| m.iter().filter(|&&c| c).count();
        assert_eq!(count(&source_change_mask(&before, &after, 0.0, 0)), 1);
        assert_eq!(count(&source_change_mask(&before, &after, 0.0, 2)), 25);
        assert_eq!(
            count(&source_change_mask(&before, &after, 0.05, 2)),
            0,
            "tiny luma change"
        );
        assert_eq!(count(&source_change_mask(&before, &before, 0.0, 3)), 0);
    }

    #[test]
    fn locked_diff_copies_untouched_pixels_from_the_previous_frame() {
        let gradient = |dot: bool| {
            let mut img = RgbaImage::from_fn(48, 32, |x, _| {
                let v = (40 + x * 4) as u8;
                Rgba([v, v, v, 255])
            });
            if dot {
                img.put_pixel(4, 2, Rgba([0, 0, 0, 255]));
            }
            AnimFrame {
                image: img,
                delay_ms: 80,
            }
        };
        let raw = vec![gradient(false), gradient(true)];
        let mut recipe = DitherRecipe::default();
        recipe.dither.algorithm = crate::dither::Algorithm::FloydSteinberg;
        let locked = dither_animation_frames(&raw, &recipe, &AnimDitherOptions::default()).unwrap();
        let opts = AnimDitherOptions {
            temporal: TemporalMode::LockedDiff,
            diff_margin: 1,
            ..AnimDitherOptions::default()
        };
        let fixed = dither_animation_frames(&raw, &recipe, &opts).unwrap();
        assert!(fixed.changed_pixels[1] <= 9, "{:?}", fixed.changed_pixels);
        assert!(locked.changed_pixels[1] > fixed.changed_pixels[1]);
        assert_eq!(fixed.frames[0].image, locked.frames[0].image);
        for (x, y, px) in fixed.frames[1].image.enumerate_pixels() {
            if x.abs_diff(4) > 1 || y.abs_diff(2) > 1 {
                assert_eq!(px, fixed.frames[0].image.get_pixel(x, y));
            }
        }
    }

    #[test]
    fn apng_blends_static_noise_over_the_canvas() {
        let mut state = 0x9e37_79b9_u32;
        let background = RgbaImage::from_fn(96, 64, |_, _| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            if state.is_multiple_of(2) {
                Rgba([0, 0, 0, 255])
            } else {
                Rgba([255, 255, 255, 255])
            }
        });
        let frames: Vec<AnimFrame> = (0..8u32)
            .map(|i| {
                let mut image = background.clone();
                for (x, y) in [(i * 10, 2), (90 - i * 10, 60)] {
                    for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                        image.put_pixel(x + dx, y + dy, Rgba([255, 0, 0, 255]));
                    }
                }
                AnimFrame {
                    image,
                    delay_ms: 80,
                }
            })
            .collect();
        assert_apng_roundtrips_exactly(&frames);
        let bytes = encode_apng(&frames, 0, false).unwrap();
        let over_frames = bytes
            .windows(4)
            .enumerate()
            .filter(|&(i, w)| w == b"fcTL" && bytes[i + 4 + 25] == 1)
            .count();
        assert!(over_frames > 0, "no frame was blended over the canvas");
    }

    #[test]
    fn apng_rejects_mixed_frame_sizes() {
        let frames = [(16, 16), (32, 16)].map(|(w, h)| AnimFrame {
            image: RgbaImage::new(w, h),
            delay_ms: 80,
        });
        let err = encode_apng(&frames, 0, false).unwrap_err().to_string();
        assert!(err.contains("frame 2 is 32x16"), "{err}");
    }

    #[test]
    fn delta_encoded_apng_with_transparency_decodes_to_identical_frames() {
        let clear = Rgba([0, 0, 0, 0]);
        let ink = Rgba([20, 30, 140, 255]);
        let wash = Rgba([200, 40, 40, 128]);
        let frames: Vec<AnimFrame> = (0..5u32)
            .map(|i| {
                let mut image = RgbaImage::from_pixel(20, 12, clear);
                for y in 2..6 {
                    for x in i.min(3) * 3..i.min(3) * 3 + 4 {
                        image.put_pixel(x, y, ink);
                    }
                }
                image.put_pixel(19, 11, wash);
                AnimFrame {
                    image,
                    delay_ms: 80,
                }
            })
            .collect();
        assert_apng_roundtrips_exactly(&frames);
    }

    #[test]
    fn delta_encoding_shrinks_static_regions() {
        // A static image with a small moving square: deltas should be far smaller than full frames.
        let frames: Vec<AnimFrame> = (0..10u32)
            .map(|i| {
                let mut img = RgbaImage::from_fn(120, 90, |x, y| {
                    if (x + y) % 2 == 0 {
                        Rgba([0, 0, 0, 255])
                    } else {
                        Rgba([255, 255, 255, 255])
                    }
                });
                for y in 10..20 {
                    for x in (i * 8)..(i * 8 + 10) {
                        img.put_pixel(x, y, Rgba([255, 255, 255, 255]));
                    }
                }
                AnimFrame {
                    image: img,
                    delay_ms: 100,
                }
            })
            .collect();
        let palette = Palette::from_hex_list("#000000,#ffffff").unwrap();
        let delta_size = assert_gif_roundtrips_exactly(&frames, &palette);
        let single = encode_gif(&frames[..1], &palette, 0, false).unwrap().len();
        assert!(
            delta_size < single * 3,
            "10 delta frames ({delta_size} B) should cost far less than 10 full frames (~{} B)",
            single * 10
        );
    }

    #[test]
    fn static_noise_background_gif_stores_only_the_motion() {
        let mut state = 0x2545_f491_u32;
        let background = RgbaImage::from_fn(96, 64, |_, _| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            if state.is_multiple_of(2) {
                Rgba([0, 0, 0, 255])
            } else {
                Rgba([255, 255, 255, 255])
            }
        });
        let frames: Vec<AnimFrame> = (0..10u32)
            .map(|i| {
                let mut image = background.clone();
                for y in 20..30 {
                    for x in i * 8..i * 8 + 10 {
                        image.put_pixel(x, y, Rgba([255, 255, 255, 255]));
                    }
                }
                AnimFrame {
                    image,
                    delay_ms: 100,
                }
            })
            .collect();
        let palette = Palette::from_hex_list("#000000,#ffffff").unwrap();
        let size = assert_gif_roundtrips_exactly(&frames, &palette);
        let single = encode_gif(&frames[..1], &palette, 0, false).unwrap().len();
        assert!(
            size < single * 2,
            "9 moving-square deltas ({size} B) should cost less than one noise frame ({single} B)"
        );
    }

    #[test]
    fn transparent_gif_deltas_decode_to_identical_frames() {
        let clear = Rgba([0, 0, 0, 0]);
        let panel = Rgba([20, 30, 140, 255]);
        let sprite = Rgba([230, 200, 40, 255]);
        let frames: Vec<AnimFrame> = (0..8u32)
            .map(|i| {
                let mut image = RgbaImage::from_pixel(48, 32, clear);
                for y in 2..10 {
                    for x in 2..20 {
                        image.put_pixel(x, y, panel);
                    }
                }
                let sx = if i < 4 { i * 6 } else { (8 - i) * 6 };
                for y in 18..26 {
                    for x in sx..sx + 6 {
                        image.put_pixel(x, y, sprite);
                    }
                }
                if i % 2 == 0 {
                    image.put_pixel(40, 4, sprite);
                }
                AnimFrame {
                    image,
                    delay_ms: 80,
                }
            })
            .collect();
        let palette = Palette::from_hex_list("#141e8c,#e6c828").unwrap();
        let bytes = encode_gif(&frames, &palette, 0, false).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("alpha.gif");
        fs::write(&path, &bytes).unwrap();
        let decoded = decode_gif_frames(&path, None, None).unwrap();
        assert_eq!(decoded.len(), frames.len());
        for (i, (dec, src)) in decoded.iter().zip(&frames).enumerate() {
            for (d, s) in dec.image.pixels().zip(src.image.pixels()) {
                if s[3] == 0 {
                    assert_eq!(d[3], 0, "frame {i} should be transparent here");
                } else {
                    assert_eq!(d.0, s.0, "frame {i}");
                }
            }
        }
        let options = {
            let mut options = gif::DecodeOptions::new();
            options.set_color_output(gif::ColorOutput::Indexed);
            options
        };
        let mut reader = options.read_info(std::io::Cursor::new(&bytes)).unwrap();
        let mut areas = Vec::new();
        while let Some(frame) = reader.read_next_frame().unwrap() {
            areas.push(u32::from(frame.width) * u32::from(frame.height));
        }
        assert_eq!(areas[0], 48 * 32);
        assert!(areas[1..].iter().all(|&a| a < 48 * 32), "{areas:?}");
    }

    #[test]
    fn ping_pong_and_holds_preserve_expected_delays() {
        let img = RgbaImage::from_pixel(4, 4, Rgba([0, 0, 0, 255]));
        let mut frames = vec![
            AnimFrame {
                image: img.clone(),
                delay_ms: 10,
            },
            AnimFrame {
                image: img.clone(),
                delay_ms: 20,
            },
            AnimFrame {
                image: img.clone(),
                delay_ms: 30,
            },
            AnimFrame {
                image: img,
                delay_ms: 40,
            },
        ];
        apply_ping_pong(&mut frames);
        apply_holds(&mut frames, 1, 2);
        let delays: Vec<u32> = frames.iter().map(|f| f.delay_ms).collect();
        assert_eq!(delays, vec![10, 10, 20, 30, 40, 30, 20, 20, 20]);
    }
}
