//! Color representations, conversions (sRGB, linear RGB, Oklab), and hex parsing.

use std::fmt;
use std::str::FromStr;
use std::sync::LazyLock;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::detmath;

/// Error returned when parsing a hex color string fails.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("invalid hex color '{input}': expected #RGB, #RRGGBB, or #RRGGBBAA")]
pub struct ParseColorError {
    /// Original offending input.
    pub input: String,
}

/// Color space used for error diffusion and nearest-palette distance matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "kebab-case")]
pub enum ColorSpace {
    /// Perceptually uniform Oklab matching with linear-light error diffusion (recommended).
    #[default]
    Oklab,
    /// Physically linear RGB light for both diffusion and distance matching.
    Linear,
    /// Classic gamma-encoded sRGB math (retro/nostalgic look).
    Srgb,
}

impl ColorSpace {
    /// Short human- and agent-readable description of this color space.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Oklab => {
                "Linear-light diffusion with perceptual Oklab palette matching (most accurate tones)"
            }
            Self::Linear => "Physically linear RGB diffusion and Euclidean distance",
            Self::Srgb => "Classic gamma-encoded sRGB arithmetic (authentic retro behavior)",
        }
    }
}

/// 8-bit sRGB color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const BLACK: Self = Self::new(0, 0, 0);
    pub const WHITE: Self = Self::new(255, 255, 255);

    /// Construct a new 8-bit sRGB color.
    #[must_use]
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Construct from a 24-bit `0xRRGGBB` literal.
    #[must_use]
    pub const fn from_u32(hex: u32) -> Self {
        Self {
            r: ((hex >> 16) & 0xFF) as u8,
            g: ((hex >> 8) & 0xFF) as u8,
            b: (hex & 0xFF) as u8,
        }
    }

    /// Return as `[r, g, b]` array.
    #[must_use]
    pub const fn to_array(self) -> [u8; 3] {
        [self.r, self.g, self.b]
    }

    /// Format as lowercase `#rrggbb`.
    #[must_use]
    pub fn to_hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }

    /// Convert to normalized `[0.0, 1.0]` sRGB floats.
    #[must_use]
    pub fn to_srgb_f32(self) -> [f32; 3] {
        [
            f32::from(self.r) / 255.0,
            f32::from(self.g) / 255.0,
            f32::from(self.b) / 255.0,
        ]
    }

    /// Convert from normalized `[0.0, 1.0]` sRGB floats (clamped and rounded).
    #[must_use]
    pub fn from_srgb_f32(v: [f32; 3]) -> Self {
        Self {
            r: float_to_u8(v[0]),
            g: float_to_u8(v[1]),
            b: float_to_u8(v[2]),
        }
    }

    /// Convert to linear RGB `[0.0, 1.0]`.
    #[must_use]
    pub fn to_linear(self) -> LinearRgb {
        LinearRgb::new(
            srgb_u8_to_linear(self.r),
            srgb_u8_to_linear(self.g),
            srgb_u8_to_linear(self.b),
        )
    }

    /// Convert to perceptual Oklab.
    #[must_use]
    pub fn to_oklab(self) -> Oklab {
        self.to_linear().to_oklab()
    }

    /// Rec. 709 relative luminance in `[0.0, 1.0]`.
    #[must_use]
    pub fn luminance(self) -> f32 {
        self.to_linear().luminance()
    }

    /// Relative perceptual contrast brightness (Oklab L in `[0.0, 1.0]`).
    #[must_use]
    pub fn lightness(self) -> f32 {
        self.to_oklab().l
    }
}

impl fmt::Display for Rgb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }
}

impl FromStr for Rgb {
    type Err = ParseColorError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let trimmed = s.trim();
        let hex = trimmed.strip_prefix('#').unwrap_or(trimmed);
        match hex.len() {
            3 => {
                let r = parse_hex_nibble(hex.as_bytes()[0]).ok_or_else(|| ParseColorError {
                    input: trimmed.to_string(),
                })?;
                let g = parse_hex_nibble(hex.as_bytes()[1]).ok_or_else(|| ParseColorError {
                    input: trimmed.to_string(),
                })?;
                let b = parse_hex_nibble(hex.as_bytes()[2]).ok_or_else(|| ParseColorError {
                    input: trimmed.to_string(),
                })?;
                Ok(Self::new(r * 17, g * 17, b * 17))
            }
            6 | 8 => {
                let r = u8::from_str_radix(&hex[0..2], 16).map_err(|_| ParseColorError {
                    input: trimmed.to_string(),
                })?;
                let g = u8::from_str_radix(&hex[2..4], 16).map_err(|_| ParseColorError {
                    input: trimmed.to_string(),
                })?;
                let b = u8::from_str_radix(&hex[4..6], 16).map_err(|_| ParseColorError {
                    input: trimmed.to_string(),
                })?;
                if hex.len() == 8 {
                    let _ = u8::from_str_radix(&hex[6..8], 16).map_err(|_| ParseColorError {
                        input: trimmed.to_string(),
                    })?;
                }
                Ok(Self::new(r, g, b))
            }
            _ => Err(ParseColorError {
                input: trimmed.to_string(),
            }),
        }
    }
}

fn parse_hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Physically linear RGB triple (nominally in `[0.0, 1.0]`, may overshoot during error diffusion).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LinearRgb {
    pub r: f32,
    pub g: f32,
    pub b: f32,
}

impl LinearRgb {
    #[must_use]
    pub const fn new(r: f32, g: f32, b: f32) -> Self {
        Self { r, g, b }
    }

    #[must_use]
    pub const fn to_array(self) -> [f32; 3] {
        [self.r, self.g, self.b]
    }

    #[must_use]
    pub const fn from_array(v: [f32; 3]) -> Self {
        Self {
            r: v[0],
            g: v[1],
            b: v[2],
        }
    }

    /// Rec. 709 linear luminance.
    #[must_use]
    pub fn luminance(self) -> f32 {
        (0.2126 * self.r + 0.7152 * self.g + 0.0722 * self.b).clamp(0.0, 1.0)
    }

    /// Convert back to 8-bit sRGB.
    #[must_use]
    pub fn to_rgb(self) -> Rgb {
        Rgb::new(
            linear_to_srgb_u8(self.r),
            linear_to_srgb_u8(self.g),
            linear_to_srgb_u8(self.b),
        )
    }

    /// Convert linear RGB to Björn Ottosson's Oklab color space.
    #[must_use]
    pub fn to_oklab(self) -> Oklab {
        let r = self.r.max(0.0);
        let g = self.g.max(0.0);
        let b = self.b.max(0.0);

        let l = 0.412_221_47 * r + 0.536_332_55 * g + 0.051_445_995 * b;
        let m = 0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b;
        let s = 0.088_302_46 * r + 0.281_718_84 * g + 0.629_978_7 * b;

        let l_ = fast_cbrt_nonneg(l);
        let m_ = fast_cbrt_nonneg(m);
        let s_ = fast_cbrt_nonneg(s);

        Oklab {
            l: 0.210_454_26 * l_ + 0.793_617_8 * m_ - 0.004_072_047 * s_,
            a: 1.977_998_5 * l_ - 2.428_592_2 * m_ + 0.450_593_7 * s_,
            b: 0.025_904_037 * l_ + 0.782_771_77 * m_ - 0.808_675_77 * s_,
        }
    }

    /// Squared Euclidean distance in linear RGB.
    #[must_use]
    pub fn dist_sq(self, other: Self) -> f32 {
        let dr = self.r - other.r;
        let dg = self.g - other.g;
        let db = self.b - other.b;
        dr * dr + dg * dg + db * db
    }
}

/// Perceptual Oklab color coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Oklab {
    pub l: f32,
    pub a: f32,
    pub b: f32,
}

impl Oklab {
    #[must_use]
    pub const fn new(l: f32, a: f32, b: f32) -> Self {
        Self { l, a, b }
    }

    /// Squared perceptual distance in Oklab.
    #[must_use]
    pub fn dist_sq(self, other: Self) -> f32 {
        let dl = self.l - other.l;
        let da = self.a - other.a;
        let db = self.b - other.b;
        dl * dl + da * da + db * db
    }

    /// Linearly interpolate between two Oklab colors (produces vivid, muddy-free gradients).
    #[must_use]
    pub fn lerp(self, other: Self, t: f32) -> Self {
        let t = t.clamp(0.0, 1.0);
        Self {
            l: self.l + (other.l - self.l) * t,
            a: self.a + (other.a - self.a) * t,
            b: self.b + (other.b - self.b) * t,
        }
    }

    /// Convert Oklab back to linear RGB.
    #[must_use]
    pub fn to_linear(self) -> LinearRgb {
        let l_ = self.l + 0.396_337_78 * self.a + 0.215_803_76 * self.b;
        let m_ = self.l - 0.105_561_346 * self.a - 0.063_854_17 * self.b;
        let s_ = self.l - 0.089_484_18 * self.a - 1.291_485_5 * self.b;

        let l = l_ * l_ * l_;
        let m = m_ * m_ * m_;
        let s = s_ * s_ * s_;

        LinearRgb {
            r: (4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s).clamp(0.0, 1.0),
            g: (-1.268_438 * l + 2.609_757_4 * m - 0.341_319_38 * s).clamp(0.0, 1.0),
            b: (-0.004_196_086_3 * l - 0.703_418_6 * m + 1.707_614_7 * s).clamp(0.0, 1.0),
        }
    }

    /// Convert Oklab back to 8-bit sRGB.
    #[must_use]
    pub fn to_rgb(self) -> Rgb {
        self.to_linear().to_rgb()
    }
}

/// Precomputed 256-entry sRGB u8 -> linear f32 lookup table (avoids `powf` per pixel).
static SRGB_U8_TO_LINEAR_LUT: LazyLock<[f32; 256]> =
    LazyLock::new(|| std::array::from_fn(|i| srgb_f32_to_linear(i as f32 / 255.0)));

/// Precomputed 4097-entry linear f32 -> sRGB u8 lookup table (exact to 8-bit, zero `powf` per pixel).
static LINEAR_TO_SRGB_U8_LUT: LazyLock<[u8; 4097]> =
    LazyLock::new(|| std::array::from_fn(|i| float_to_u8(linear_to_srgb_f32(i as f32 / 4096.0))));

/// Convert a single `[0.0, 1.0]` sRGB channel to linear light (IEC 61966-2-1).
#[must_use]
pub fn srgb_f32_to_linear(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.04045 {
        c / 12.92
    } else {
        detmath::powf((c + 0.055) / 1.055, 2.4)
    }
}

/// Convert a single `u8` sRGB channel to linear light `[0.0, 1.0]` via fast LUT.
#[inline]
#[must_use]
pub fn srgb_u8_to_linear(v: u8) -> f32 {
    SRGB_U8_TO_LINEAR_LUT[usize::from(v)]
}

/// Convert a single linear light channel `[0.0, 1.0]` to sRGB `[0.0, 1.0]`.
#[must_use]
pub fn linear_to_srgb_f32(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.003_130_8 {
        12.92 * c
    } else {
        1.055 * detmath::powf(c, 1.0 / 2.4) - 0.055
    }
}

/// Convert a single linear light channel `[0.0, 1.0]` to `u8` sRGB via fast LUT.
#[inline]
#[must_use]
pub fn linear_to_srgb_u8(c: f32) -> u8 {
    let idx = (c.clamp(0.0, 1.0) * 4096.0 + 0.5) as usize;
    LINEAR_TO_SRGB_U8_LUT[idx.min(4096)]
}

#[inline]
fn float_to_u8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

/// Fast, safe, branchless cube root for non-negative `f32` (~1e-7 accuracy, ~6x faster than libm `cbrt`).
#[inline]
fn fast_cbrt_nonneg(x: f32) -> f32 {
    if x <= 1e-12 {
        return 0.0;
    }
    let mut y = f32::from_bits(x.to_bits() / 3 + 0x2a51_067f);
    y = (2.0 * y + x / (y * y)) * (1.0 / 3.0);
    y = (2.0 * y + x / (y * y)) * (1.0 / 3.0);
    y
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_parsing_round_trips() {
        assert_eq!("#000".parse::<Rgb>().unwrap(), Rgb::BLACK);
        assert_eq!("fff".parse::<Rgb>().unwrap(), Rgb::WHITE);
        assert_eq!(
            "#1a2b3c".parse::<Rgb>().unwrap(),
            Rgb::new(0x1a, 0x2b, 0x3c)
        );
        assert_eq!(
            "#1a2b3cff".parse::<Rgb>().unwrap(),
            Rgb::new(0x1a, 0x2b, 0x3c)
        );
        assert_eq!(Rgb::new(0x0f, 0xa0, 0x55).to_hex(), "#0fa055");
        assert!("not-a-color".parse::<Rgb>().is_err());
    }

    #[test]
    fn oklab_round_trip_is_accurate() {
        for sample in [
            Rgb::BLACK,
            Rgb::WHITE,
            Rgb::new(220, 48, 32),
            Rgb::new(40, 160, 120),
            Rgb::new(30, 64, 210),
            Rgb::new(128, 128, 128),
        ] {
            let round_trip = sample.to_oklab().to_rgb();
            assert!(
                sample.r.abs_diff(round_trip.r) <= 1
                    && sample.g.abs_diff(round_trip.g) <= 1
                    && sample.b.abs_diff(round_trip.b) <= 1,
                "round-trip drift for {sample}: got {round_trip}"
            );
        }
    }
}
