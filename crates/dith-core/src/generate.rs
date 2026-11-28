//! Procedural artwork generators so agents can create dithered graphics from scratch.

use std::f32::consts::TAU;
use std::sync::Arc;

use image::RgbaImage;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::color::{Oklab, Rgb};
use crate::detmath;
use crate::inflate::{InflateParams, InflateShape, inflate_tones};
use crate::palette::SplitMix64;

/// Built-in procedural source patterns fed into the dithering pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "kebab-case")]
pub enum Pattern {
    /// Smooth multi-stop linear gradient interpolated in Oklab.
    #[default]
    LinearGradient,
    /// Glowing radial spotlight / orb gradient interpolated in Oklab.
    RadialGradient,
    /// Organic multi-anchor smooth aurora/mesh gradient.
    Mesh,
    /// Softly lit 3D matte/specular sphere on a shaded studio backdrop.
    Sphere,
    /// Layered topographic silk interference waves.
    Waves,
    /// Multi-octave fractal Brownian motion (fBm) organic clouds/terrain.
    Noise,
    /// Concentric rippled wave rings.
    Rings,
    /// Smooth multi-frequency psychedelic plasma field.
    Plasma,
    /// Soft shaded geometric perspective checkerboard.
    Checkerboard,
    /// A drawn shape (`--shape` SVG or PNG) puffed up like a pillow and lit like `sphere`.
    Inflate,
}

/// Static metadata for discovering generators via `list` / `capabilities`.
#[derive(Debug, Clone, Serialize)]
pub struct PatternInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub best_for: &'static str,
    /// What moves when the pattern is animated with `dith animate <pattern>`.
    pub motion: &'static str,
}

impl Pattern {
    pub const ALL: &'static [Self] = &[
        Self::LinearGradient,
        Self::RadialGradient,
        Self::Mesh,
        Self::Sphere,
        Self::Waves,
        Self::Noise,
        Self::Rings,
        Self::Plasma,
        Self::Checkerboard,
        Self::Inflate,
    ];

    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::LinearGradient => "linear-gradient",
            Self::RadialGradient => "radial-gradient",
            Self::Mesh => "mesh",
            Self::Sphere => "sphere",
            Self::Waves => "waves",
            Self::Noise => "noise",
            Self::Rings => "rings",
            Self::Plasma => "plasma",
            Self::Checkerboard => "checkerboard",
            Self::Inflate => "inflate",
        }
    }

    #[must_use]
    pub const fn info(self) -> PatternInfo {
        match self {
            Self::LinearGradient => PatternInfo {
                id: "linear-gradient",
                name: "Linear Gradient",
                description: "Smooth directional Oklab gradient across arbitrary color stops",
                best_for: "Hero backgrounds, banners, cards, testing dithering tonal smoothness",
                motion: "Gradient direction rotates a full 360 degrees per loop (times --motion-amplitude).",
            },
            Self::RadialGradient => PatternInfo {
                id: "radial-gradient",
                name: "Radial Glow",
                description: "Atmospheric radial gradient with configurable focal center",
                best_for: "Spotlight backdrops, glowing orbs, editorial covers",
                motion: "Glow center orbits a small circle (8% of the canvas) while its radius gently pulses.",
            },
            Self::Mesh => PatternInfo {
                id: "mesh",
                name: "Aurora Mesh",
                description: "Seeded multi-anchor organic mesh gradient in Oklab space",
                best_for: "Modern SaaS/AI hero visuals, album art, abstract posters",
                motion: "Each color anchor orbits its own small circle, alternating directions, so the colors drift and blend.",
            },
            Self::Sphere => PatternInfo {
                id: "sphere",
                name: "Lit 3D Sphere",
                description: "Analytically shaded 3D sphere with rim light and studio vignette",
                best_for: "Showcasing Atkinson, Halftone, Blue Noise, and Risograph aesthetics",
                motion: "Sphere stays still; the key light orbits 360 degrees around it, moving the highlight and terminator. --motion-amplitude only changes the light's tilt wobble.",
            },
            Self::Waves => PatternInfo {
                id: "waves",
                name: "Silk Waves",
                description: "Layered sinusoidal interference ribbons",
                best_for: "Editorial dividers, kinetic posters, woodcut & scanline dithers",
                motion: "Ribbons flow and undulate, advancing one full wavelength per loop.",
            },
            Self::Noise => PatternInfo {
                id: "noise",
                name: "Organic fBm Noise",
                description: "Multi-octave smooth value-noise clouds and topographic fields",
                best_for: "Textured backgrounds, subtle paper/mist art, blue-noise stipples",
                motion: "Cloud field drifts along a circular path, with alternate octaves moving in opposite directions so it billows.",
            },
            Self::Rings => PatternInfo {
                id: "rings",
                name: "Concentric Rings",
                description: "Rippled radial wave rings radiating from a focal point",
                best_for: "Hypnotic zine covers, optical posters, Bayer & halftone tests",
                motion: "Rings ripple outward from the center by round(--motion-amplitude) wavelengths per loop.",
            },
            Self::Plasma => PatternInfo {
                id: "plasma",
                name: "Smooth Plasma",
                description: "Classic multi-frequency continuous plasma field",
                best_for: "Retro demos, synthwave backdrops, PICO-8 and cyber-magenta presets",
                motion: "Plasma field cycles through round(--motion-amplitude) full periods per loop.",
            },
            Self::Checkerboard => PatternInfo {
                id: "checkerboard",
                name: "Shaded Checkerboard",
                description: "Soft-lit geometric grid with subtle radial vignette",
                best_for: "Architectural graphics, retro UI tiles, graphic posters",
                motion: "Grid scrolls diagonally 2 x round(--motion-amplitude) cells per loop while the vignette light orbits.",
            },
            Self::Inflate => PatternInfo {
                id: "inflate",
                name: "Inflated Shape",
                description: "Your own --shape (SVG or PNG) puffed up from its distance field like a pillow, several shapes melted together with --blend, lit like sphere with an optional ground shadow and --paint tones",
                best_for: "Custom characters, mascots, icons and logos with soft 3D shading",
                motion: "Shape stays still; the key light orbits 360 degrees around it like sphere. Pass --cycles 0 to keep the light still (for blinks driven by --overlay-frames).",
            },
        }
    }
}

/// Parameters controlling procedural pattern generation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GenerateOptions {
    pub pattern: Pattern,
    pub width: u32,
    pub height: u32,
    /// Angle in degrees for directional patterns (`linear-gradient`, `waves`).
    pub angle: f32,
    /// Frequency / scale multiplier (`1.0` = balanced default).
    pub frequency: f32,
    /// Color stops interpolated in Oklab (defaults to dark -> light smooth ramp if empty).
    pub stops: Vec<Rgb>,
    /// Deterministic seed for randomized patterns (`mesh`, `noise`).
    pub seed: u64,
    /// Seamless animation loop phase in `[0.0, 1.0)` (`0.0` == `1.0`).
    #[serde(default)]
    pub phase: f32,
    /// Scales procedural motion travel/orbit amplitude (`0.0` = still, `1.0` = default, `2.0` = dramatic).
    #[serde(default = "default_motion_amplitude")]
    pub motion_amplitude: f32,
    /// Form and lighting of the `inflate` pattern.
    #[serde(default)]
    pub inflate: InflateParams,
    /// Shapes for the `inflate` pattern, at this canvas size (a centred disc when `None`).
    #[serde(skip)]
    pub shape: Option<Arc<InflateShape>>,
}

const fn default_motion_amplitude() -> f32 {
    1.0
}

impl Default for GenerateOptions {
    fn default() -> Self {
        Self {
            pattern: Pattern::Sphere,
            width: 640,
            height: 480,
            angle: 135.0,
            frequency: 1.0,
            stops: vec![Rgb::from_u32(0x0F172A), Rgb::from_u32(0xF8FAFC)],
            seed: 42,
            phase: 0.0,
            motion_amplitude: 1.0,
            inflate: InflateParams::default(),
            shape: None,
        }
    }
}

/// Render a procedural source image ready to enter the preprocess + dither pipeline.
#[must_use]
pub fn generate_pattern(opts: &GenerateOptions) -> RgbaImage {
    let w = opts.width.max(1);
    let h = opts.height.max(1);
    let phase = opts.phase.rem_euclid(1.0);
    let amp = opts.motion_amplitude.clamp(0.0, 4.0);
    let stops: Vec<Oklab> = if opts.stops.len() >= 2 {
        opts.stops.iter().map(|c| c.to_oklab()).collect()
    } else {
        vec![Rgb::BLACK.to_oklab(), Rgb::WHITE.to_oklab()]
    };

    match opts.pattern {
        Pattern::LinearGradient => render_linear(w, h, opts.angle + phase * 360.0 * amp, &stops),
        Pattern::RadialGradient => render_radial(w, h, opts.frequency, phase, amp, &stops),
        Pattern::Mesh => render_mesh(w, h, opts.seed, phase, amp, &stops),
        Pattern::Sphere => render_sphere(w, h, opts.angle, phase, amp, &stops),
        Pattern::Waves => render_waves(w, h, opts.angle, opts.frequency, phase, amp, &stops),
        Pattern::Noise => render_noise(w, h, opts.frequency, opts.seed, phase, amp, &stops),
        Pattern::Rings => render_rings(w, h, opts.frequency, phase, amp, &stops),
        Pattern::Plasma => render_plasma(w, h, opts.frequency, phase, amp, &stops),
        Pattern::Checkerboard => render_checkerboard(w, h, opts.frequency, phase, amp, &stops),
        Pattern::Inflate => {
            let fallback;
            let shape = match &opts.shape {
                Some(s) if (s.width, s.height) == (w, h) => s.as_ref(),
                _ => {
                    fallback = InflateShape::default_disc(w, h);
                    &fallback
                }
            };
            let tones = inflate_tones(shape, opts.angle, phase, amp, &opts.inflate);
            render_parallel(w, h, |x, y| {
                sample_stops(&stops, tones[(y * w + x) as usize])
            })
        }
    }
}

fn sample_stops(stops: &[Oklab], t: f32) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    if stops.len() == 1 {
        return stops[0].to_rgb();
    }
    let scaled = t * (stops.len() - 1) as f32;
    let idx = (scaled.floor() as usize).min(stops.len() - 2);
    let local = scaled - idx as f32;
    stops[idx].lerp(stops[idx + 1], local).to_rgb()
}

fn render_parallel<F>(w: u32, h: u32, pixel_fn: F) -> RgbaImage
where
    F: Fn(u32, u32) -> Rgb + Sync,
{
    let w_usize = w as usize;
    let row_bytes = w_usize * 4;
    let mut raw = vec![255u8; row_bytes * (h as usize)];
    raw.par_chunks_exact_mut(row_bytes)
        .enumerate()
        .for_each(|(y_idx, row)| {
            let y = y_idx as u32;
            let (pixels, _) = row.as_chunks_mut::<4>();
            for (x_idx, px) in pixels.iter_mut().enumerate() {
                let c = pixel_fn(x_idx as u32, y);
                px[0] = c.r;
                px[1] = c.g;
                px[2] = c.b;
            }
        });
    RgbaImage::from_raw(w, h, raw).unwrap_or_else(|| RgbaImage::new(w, h))
}

fn render_linear(w: u32, h: u32, angle_deg: f32, stops: &[Oklab]) -> RgbaImage {
    let rad = angle_deg.to_radians();
    let dx = detmath::cos(rad);
    let dy = detmath::sin(rad);
    let span = dx.abs() + dy.abs();
    let inv_span = if span > 1e-5 { 1.0 / span } else { 1.0 };

    let wf = (w - 1).max(1) as f32;
    let hf = (h - 1).max(1) as f32;
    render_parallel(w, h, |x, y| {
        let u = x as f32 / wf - 0.5;
        let v = y as f32 / hf - 0.5;
        let t = (u * dx + v * dy) * inv_span + 0.5;
        sample_stops(stops, t)
    })
}

fn render_radial(
    w: u32,
    h: u32,
    frequency: f32,
    phase: f32,
    amp: f32,
    stops: &[Oklab],
) -> RgbaImage {
    let theta = phase * TAU;
    let pulse = if phase == 0.0 {
        1.0
    } else {
        1.0 + 0.16 * amp * detmath::sin(theta)
    };
    let scale = (frequency * pulse).clamp(0.2, 5.0);
    let orbit_x = if phase == 0.0 {
        0.0
    } else {
        (w as f32) * 0.08 * amp * detmath::cos(theta)
    };
    let orbit_y = if phase == 0.0 {
        0.0
    } else {
        (h as f32) * 0.08 * amp * detmath::sin(theta)
    };
    let cx = (w as f32 - 1.0) * 0.5 + orbit_x;
    let cy = (h as f32 - 1.0) * 0.5 + orbit_y;
    let radius = detmath::hypot((w as f32 - 1.0) * 0.5, (h as f32 - 1.0) * 0.5).max(1.0) / scale;

    render_parallel(w, h, |x, y| {
        let dist = detmath::hypot(x as f32 - cx, y as f32 - cy) / radius;
        // Invert so bright highlight sits at center when stops go dark->light.
        let t = (1.0 - dist).clamp(0.0, 1.0);
        let smooth = t * t * (3.0 - 2.0 * t);
        sample_stops(stops, smooth)
    })
}

fn render_mesh(w: u32, h: u32, seed: u64, phase: f32, amp: f32, stops: &[Oklab]) -> RgbaImage {
    let mut rng = SplitMix64::new(seed ^ 0xAE54_1234_5678_9ABC);
    let anchor_count = stops.len().clamp(4, 8);
    let theta = phase * TAU;
    let mut anchors: Vec<(f32, f32, Oklab)> = Vec::with_capacity(anchor_count);
    for i in 0..anchor_count {
        let base_x = 0.08 + rng.next_f32() * 0.84;
        let base_y = 0.08 + rng.next_f32() * 0.84;
        let orbit_r = (0.14 + rng.next_f32() * 0.12) * amp;
        let orbit_phi = rng.next_f32() * TAU;
        let dir = if i % 2 == 0 { 1.0 } else { -1.0 };
        let (ax, ay) = if phase == 0.0 {
            (base_x, base_y)
        } else {
            let ox = (base_x + orbit_r * detmath::cos(dir * theta + orbit_phi)
                - orbit_r * detmath::cos(orbit_phi))
            .clamp(0.02, 0.98);
            let oy = (base_y + orbit_r * detmath::sin(dir * theta + orbit_phi)
                - orbit_r * detmath::sin(orbit_phi))
            .clamp(0.02, 0.98);
            (ox, oy)
        };
        let t = i as f32 / (anchor_count - 1).max(1) as f32;
        let scaled = t * (stops.len() - 1) as f32;
        let idx = (scaled.floor() as usize).min(stops.len() - 2);
        let color = stops[idx].lerp(stops[idx + 1], scaled - idx as f32);
        anchors.push((ax, ay, color));
    }

    let wf = (w - 1).max(1) as f32;
    let hf = (h - 1).max(1) as f32;
    render_parallel(w, h, |x, y| {
        let u = x as f32 / wf;
        let v = y as f32 / hf;
        let mut l = 0.0f32;
        let mut a = 0.0f32;
        let mut b = 0.0f32;
        let mut wsum = 0.0f32;
        for &(ax, ay, col) in &anchors {
            let d2 = (u - ax) * (u - ax) + (v - ay) * (v - ay);
            let wt = 1.0 / (d2 + 0.045);
            l += col.l * wt;
            a += col.a * wt;
            b += col.b * wt;
            wsum += wt;
        }
        Oklab::new(l / wsum, a / wsum, b / wsum).to_rgb()
    })
}

fn render_sphere(
    w: u32,
    h: u32,
    angle_deg: f32,
    phase: f32,
    amp: f32,
    stops: &[Oklab],
) -> RgbaImage {
    let cx = (w as f32 - 1.0) * 0.5;
    let cy = (h as f32 - 1.0) * 0.5;
    let inv_r = 1.0 / ((w.min(h) as f32) * 0.36).max(1.0);
    let inv_hm1 = 1.0 / (h - 1).max(1) as f32;

    // Key light orbits smoothly over phase in [0, 1) with gentle latitude tilt.
    let rad = (angle_deg + phase * 360.0).to_radians();
    let tilt = if phase == 0.0 {
        0.65
    } else {
        (0.65 + 0.14 * amp * detmath::sin(phase * TAU)).clamp(0.15, 0.92)
    };
    let lx = detmath::cos(rad) * tilt;
    let ly = -detmath::sin(rad) * tilt;
    let lz = (1.0 - lx * lx - ly * ly).max(0.1).sqrt();
    let hz = lz + 1.0;
    let inv_hlen = 1.0 / (lx * lx + ly * ly + hz * hz).sqrt();

    render_parallel(w, h, |x, y| {
        let dx = (x as f32 - cx) * inv_r;
        let dy = (y as f32 - cy) * inv_r;
        let d2 = dx * dx + dy * dy;

        let val = if d2 <= 1.0 {
            let nz = (1.0 - d2).sqrt();
            let diffuse = (dx * lx + dy * ly + nz * lz).max(0.0);
            // Blinn-Phong soft specular highlight.
            let spec = detmath::powi(((dx * lx + dy * ly + nz * hz) * inv_hlen).max(0.0), 28);
            // Gentle Fresnel rim light so dark edge separates cleanly from backdrop.
            let rim = detmath::powf(1.0 - nz, 2.6) * 0.28;
            highlight_shoulder(0.08 + diffuse * 0.78 + spec * 0.42 + rim)
        } else {
            // Subtle studio backdrop gradient + soft floor glow.
            let bg_y = y as f32 * inv_hm1;
            let halo = detmath::exp(-((d2.sqrt() - 1.0) * 2.2).powi(2)) * 0.14;
            (0.16 * (1.0 - bg_y) + 0.04 + halo).clamp(0.0, 1.0)
        };

        sample_stops(stops, val)
    })
}

/// Soft highlight roll-off: values above the knee compress smoothly towards 1.0 instead of
/// clipping, so bright highlights keep tonal detail rather than becoming a flat patch.
pub(crate) fn highlight_shoulder(v: f32) -> f32 {
    const KNEE: f32 = 0.78;
    if v <= KNEE {
        return v.max(0.0);
    }
    let headroom = 1.0 - KNEE;
    (KNEE + headroom * (1.0 - detmath::exp(-(v - KNEE) / headroom))).min(1.0)
}

fn render_waves(
    w: u32,
    h: u32,
    angle_deg: f32,
    frequency: f32,
    phase: f32,
    amp: f32,
    stops: &[Oklab],
) -> RgbaImage {
    let rad = angle_deg.to_radians();
    let ca = detmath::cos(rad);
    let sa = detmath::sin(rad);
    let freq = frequency.clamp(0.2, 10.0);
    let theta = phase * TAU;

    let wf = (w - 1).max(1) as f32;
    let hf = (h - 1).max(1) as f32;
    render_parallel(w, h, |x, y| {
        let u = x as f32 / wf - 0.5;
        let v = y as f32 / hf - 0.5;
        let ru = u * ca - v * sa;
        let rv = u * sa + v * ca;
        let wave1 = detmath::sin(
            (ru * 3.0 * freq + detmath::sin(rv * 2.5 * freq + theta) * 0.45 * amp) * TAU - theta,
        );
        let wave2 = detmath::cos(
            (rv * 1.8 * freq - detmath::cos(ru * 2.0 * freq - theta) * 0.35 * amp) * TAU + theta,
        );
        let t = (0.5 + 0.35 * wave1 + 0.15 * wave2).clamp(0.0, 1.0);
        sample_stops(stops, t)
    })
}

fn render_noise(
    w: u32,
    h: u32,
    frequency: f32,
    seed: u64,
    phase: f32,
    amp: f32,
    stops: &[Oklab],
) -> RgbaImage {
    let scale = 4.0 * frequency.clamp(0.2, 8.0);
    let min_side = w.min(h).max(1) as f32;
    let theta = phase * TAU;
    let loop_r = 0.85f32 * amp;
    let dx0 = if phase == 0.0 {
        0.0
    } else {
        loop_r * (detmath::cos(theta) - 1.0)
    };
    let dy0 = if phase == 0.0 {
        0.0
    } else {
        loop_r * detmath::sin(theta)
    };

    render_parallel(w, h, |x, y| {
        let ux = (x as f32 / min_side) * scale;
        let uy = (y as f32 / min_side) * scale;
        let mut val = 0.0f32;
        let mut oct_amp = 0.5f32;
        let mut freq = 1.0f32;
        let mut norm = 0.0f32;
        for oct in 0..5u64 {
            let sign = if oct % 2 == 0 { 1.0 } else { -1.0 };
            let sx = ux * freq + dx0 * sign;
            let sy = uy * freq + dy0 * sign;
            val += value_noise_2d(sx, sy, seed.wrapping_add(oct * 1013)) * oct_amp;
            norm += oct_amp;
            oct_amp *= 0.5;
            freq *= 2.02;
        }
        let t = (val / norm).clamp(0.0, 1.0);
        // Gentle S-curve so clouds have dramatic highlights and deep shadows.
        let contrasted = t * t * (3.0 - 2.0 * t);
        sample_stops(stops, contrasted)
    })
}

fn render_rings(
    w: u32,
    h: u32,
    frequency: f32,
    phase: f32,
    amp: f32,
    stops: &[Oklab],
) -> RgbaImage {
    let cx = (w as f32 - 1.0) * 0.5;
    let cy = (h as f32 - 1.0) * 0.5;
    let norm = w.min(h).max(1) as f32 * 0.5;
    let freq = 4.0 * frequency.clamp(0.2, 10.0);
    let wave_step = amp.round().max(1.0);

    render_parallel(w, h, |x, y| {
        let dist = detmath::hypot(x as f32 - cx, y as f32 - cy) / norm;
        let ripple = 0.5 + 0.5 * detmath::cos((dist * freq - phase * wave_step) * TAU);
        let fade = detmath::exp(-dist * 0.45);
        let t = (ripple * (0.45 + 0.55 * fade)).clamp(0.0, 1.0);
        sample_stops(stops, t)
    })
}

fn render_plasma(
    w: u32,
    h: u32,
    frequency: f32,
    phase: f32,
    amp: f32,
    stops: &[Oklab],
) -> RgbaImage {
    let freq = frequency.clamp(0.2, 6.0);
    let wf = (w - 1).max(1) as f32;
    let hf = (h - 1).max(1) as f32;
    let theta = phase * TAU * amp.round().max(1.0);

    render_parallel(w, h, |x, y| {
        let u = (x as f32 / wf) * TAU * freq;
        let v = (y as f32 / hf) * TAU * freq;
        let p1 = detmath::sin(u + theta);
        let p2 = detmath::cos(v * 1.15 - theta);
        let p3 = detmath::sin((u + v) * 0.7 + theta);
        let p4 =
            detmath::sin(detmath::hypot(u - 0.5 * TAU * freq, v - 0.5 * TAU * freq) * 1.2 - theta);
        let t = ((p1 + p2 + p3 + p4) * 0.125 + 0.5).clamp(0.0, 1.0);
        sample_stops(stops, t)
    })
}

fn render_checkerboard(
    w: u32,
    h: u32,
    frequency: f32,
    phase: f32,
    amp: f32,
    stops: &[Oklab],
) -> RgbaImage {
    let cells = (8.0 * frequency.clamp(0.25, 8.0)).max(2.0);
    let min_side = w.min(h).max(1) as f32;
    let theta = phase * TAU;
    let cx = (w as f32 - 1.0) * 0.5
        + if phase == 0.0 {
            0.0
        } else {
            (w as f32) * 0.12 * amp * detmath::cos(theta)
        };
    let cy = (h as f32 - 1.0) * 0.5
        + if phase == 0.0 {
            0.0
        } else {
            (h as f32) * 0.12 * amp * detmath::sin(theta)
        };
    let max_r = detmath::hypot((w as f32 - 1.0) * 0.5, (h as f32 - 1.0) * 0.5).max(1.0);
    let scroll = phase * 2.0 * amp.round().max(1.0);

    render_parallel(w, h, |x, y| {
        let gx = ((x as f32 / min_side) * cells + scroll).floor() as i32;
        let gy = ((y as f32 / min_side) * cells + scroll).floor() as i32;
        let is_light = (gx + gy).rem_euclid(2) == 0;
        let vig = 1.0 - 0.38 * (detmath::hypot(x as f32 - cx, y as f32 - cy) / max_r);
        let base = if is_light { 0.92 } else { 0.14 };
        let t = (base * vig).clamp(0.0, 1.0);
        sample_stops(stops, t)
    })
}

fn hash_2d(ix: i32, iy: i32, seed: u64) -> f32 {
    let mut z = (ix as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (iy as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9)
        ^ seed.wrapping_mul(0x94D0_49BB_1331_11EB);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    (z >> 40) as f32 * (1.0 / ((1u32 << 24) as f32))
}

fn value_noise_2d(x: f32, y: f32, seed: u64) -> f32 {
    let ix = x.floor() as i32;
    let iy = y.floor() as i32;
    let fx = x - ix as f32;
    let fy = y - iy as f32;
    let ux = fx * fx * (3.0 - 2.0 * fx);
    let uy = fy * fy * (3.0 - 2.0 * fy);

    let a = hash_2d(ix, iy, seed);
    let b = hash_2d(ix + 1, iy, seed);
    let c = hash_2d(ix, iy + 1, seed);
    let d = hash_2d(ix + 1, iy + 1, seed);

    let top = a + (b - a) * ux;
    let bot = c + (d - c) * ux;
    top + (bot - top) * uy
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_patterns_render_requested_dimensions() {
        for &pattern in Pattern::ALL {
            let opts = GenerateOptions {
                pattern,
                width: 48,
                height: 32,
                ..Default::default()
            };
            let img = generate_pattern(&opts);
            assert_eq!(img.dimensions(), (48, 32), "pattern {pattern:?}");
        }
    }
}
