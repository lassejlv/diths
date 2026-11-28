//! Image export (PNG/WebP/JPEG/GIF/BMP/TIFF, vector SVG, Kitty graphics, Braille, ASCII) and stats.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::fs;
use std::hash::{BuildHasherDefault, Hasher};
use std::io::{self, Cursor, IsTerminal};
use std::path::Path;

use anyhow::{Context, Result};
use image::{DynamicImage, ImageFormat, RgbaImage};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::color::Rgb;
use crate::deflate::Effort;
use crate::png_optimize::write_chunk;

/// Maximum Base64 payload bytes per Kitty graphics protocol chunk (per Kitty spec).
const KITTY_CHUNK_SIZE: usize = 4096;

/// Supported output file and terminal formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "kebab-case")]
pub enum OutputFormat {
    /// Lossless PNG (recommended for crisp pixel-exact dithers).
    #[default]
    Png,
    /// Lossless WebP.
    Webp,
    /// JPEG (flattens alpha).
    Jpeg,
    /// Animated-capable GIF (crisp indexed output).
    Gif,
    /// Lossless BMP (palettized 1/4/8-bit when opaque with at most 256 colors).
    Bmp,
    /// Lossless Deflate-compressed TIFF.
    Tiff,
    /// Crisp vector SVG (run-length merged rects or halftone circles/dots).
    Svg,
    /// Pixel-exact inline terminal preview using the Kitty graphics protocol (Kitty, Ghostty, `WezTerm`).
    Kitty,
    /// High-density Unicode Braille text art (2x4 subpixels per glyph).
    Braille,
    /// Classic shaded ASCII text art.
    Ascii,
}

/// Shape primitives used when exporting `--format svg`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "kebab-case")]
pub enum SvgShape {
    /// Run-length merged crisp pixel runs, layered by color, horizontal or vertical per color
    /// (smallest file size).
    #[default]
    Rect,
    /// Circular stipple/halftone dots per non-background pixel.
    Circle,
    /// Slightly inset rounded dots with delicate spacing.
    Dot,
}

/// Options controlling Kitty graphics protocol terminal rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KittyOptions {
    /// Optional display width in terminal character columns (`c=`).
    pub cols: Option<u32>,
    /// Optional display height in terminal character rows (`r=`).
    pub rows: Option<u32>,
    /// Wrap escape sequences in tmux DCS passthrough (`\x1bPtmux;...\x1b\\`).
    pub tmux_passthrough: bool,
    /// Append a trailing newline after the final chunk (disable for exact cursor placement in fullscreen).
    pub trailing_newline: bool,
    /// Emit `C=1` so the terminal does not advance the text cursor after placing the image.
    pub no_cursor_move: bool,
    /// Optional stable Kitty image ID (`i=N,p=1`) for flicker-free atomic in-place frame updates.
    pub image_id: Option<u32>,
}

impl Default for KittyOptions {
    fn default() -> Self {
        Self {
            cols: None,
            rows: None,
            tmux_passthrough: false,
            trailing_newline: true,
            no_cursor_move: false,
            image_id: None,
        }
    }
}

impl KittyOptions {
    /// Enable tmux passthrough automatically only when running interactively inside tmux.
    #[must_use]
    pub fn should_use_tmux_passthrough() -> bool {
        std::env::var_os("TMUX").is_some() && io::stdout().is_terminal()
    }
}

/// Escape sequence that clears all Kitty graphics placements from the current screen.
#[must_use]
pub const fn kitty_clear_sequence(tmux_passthrough: bool) -> &'static str {
    if tmux_passthrough {
        "\x1bPtmux;\x1b\x1b_Ga=d,d=A,q=2\x1b\x1b\\\x1b\\"
    } else {
        "\x1b_Ga=d,d=A,q=2\x1b\\"
    }
}

/// Options controlling vector SVG, Kitty graphics, and text rendering.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ExportOptions {
    pub format: Option<OutputFormat>,
    pub svg_shape: SvgShape,
    /// Skip emitting SVG shapes for the most frequent color (or explicit transparent background).
    pub svg_skip_bg: bool,
    /// Maximum character columns when rendering `braille` or `ascii`.
    pub text_width: Option<u32>,
    /// Display width in terminal columns when rendering inline with Kitty graphics (`c=`).
    pub view_cols: Option<u32>,
    /// Display height in terminal rows when rendering inline with Kitty graphics (`r=`).
    pub view_rows: Option<u32>,
    /// Spend extra CPU on the smallest lossless PNG/APNG (palette, filter, and Zopfli search), and
    /// on JPEG Huffman tables fitted to the image (the decoded pixels stay the same).
    #[serde(default)]
    pub optimize: bool,
}

/// Per-color usage statistics returned in structured JSON output.
#[derive(Debug, Clone, Serialize)]
pub struct ColorUsage {
    pub hex: String,
    pub pixels: u64,
    pub percent: f64,
}

impl OutputFormat {
    /// Infer format from file extension when `--format` is not explicitly specified.
    #[must_use]
    pub fn infer_from_path(path: &Path) -> Self {
        match path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("webp") => Self::Webp,
            Some("jpg" | "jpeg") => Self::Jpeg,
            Some("gif") => Self::Gif,
            Some("bmp") => Self::Bmp,
            Some("tif" | "tiff") => Self::Tiff,
            Some("svg") => Self::Svg,
            Some("kitty") => Self::Kitty,
            Some("braille") => Self::Braille,
            Some("txt" | "asc" | "ascii") => Self::Ascii,
            _ => Self::Png,
        }
    }

    /// Canonical CLI slug.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Webp => "webp",
            Self::Jpeg => "jpeg",
            Self::Gif => "gif",
            Self::Bmp => "bmp",
            Self::Tiff => "tiff",
            Self::Svg => "svg",
            Self::Kitty => "kitty",
            Self::Braille => "braille",
            Self::Ascii => "ascii",
        }
    }

    /// Whether this format emits UTF-8 text or terminal escape sequences directly to stdout.
    #[must_use]
    pub const fn is_text(self) -> bool {
        matches!(self, Self::Svg | Self::Kitty | Self::Braille | Self::Ascii)
    }
}

/// Compute palette color usage histogram (opaque pixels) sorted by descending frequency.
#[must_use]
pub fn color_histogram(img: &RgbaImage) -> Vec<ColorUsage> {
    type Counts = HashMap<u32, u64, BuildHasherDefault<PixelKeyHasher>>;
    let add = |mut into: Counts, from: Counts| {
        for (key, pixels) in from {
            *into.entry(key).or_insert(0) += pixels;
        }
        into
    };
    let counts = img
        .as_raw()
        .par_chunks(4 * 16_384)
        .fold(Counts::default, |mut counts, chunk| {
            // Dithered output repeats a few colors, so count runs before touching the map.
            let mut run_key = 0u32;
            let mut run = 0u64;
            for px in chunk.as_chunks::<4>().0 {
                if px[3] == 0 {
                    continue;
                }
                let key = u32::from_be_bytes([0, px[0], px[1], px[2]]);
                if key == run_key {
                    run += 1;
                } else {
                    if run > 0 {
                        *counts.entry(run_key).or_insert(0) += run;
                    }
                    run_key = key;
                    run = 1;
                }
            }
            if run > 0 {
                *counts.entry(run_key).or_insert(0) += run;
            }
            counts
        })
        .reduce(Counts::default, add);
    let total: u64 = counts.values().sum();
    let denom = total.max(1) as f64;
    let mut rows: Vec<ColorUsage> = counts
        .into_iter()
        .map(|(key, pixels)| {
            let [_, r, g, b] = key.to_be_bytes();
            ColorUsage {
                hex: Rgb::new(r, g, b).to_hex(),
                pixels,
                percent: ((pixels as f64 / denom) * 10_000.0).round() / 100.0,
            }
        })
        .collect();
    rows.sort_by(|a, b| b.pixels.cmp(&a.pixels).then_with(|| a.hex.cmp(&b.hex)));
    rows
}

/// Hasher for packed `0x00RRGGBB` keys: one multiply, folded so every input bit reaches the
/// low bits the table indexes by.
#[derive(Default)]
struct PixelKeyHasher(u64);

impl Hasher for PixelKeyHasher {
    fn finish(&self) -> u64 {
        self.0 ^ (self.0 >> 29)
    }

    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.write_u32(u32::from(byte));
        }
    }

    fn write_u32(&mut self, value: u32) {
        self.0 = (self.0 ^ u64::from(value)).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }
}

/// Encode `img` into raw bytes in the requested format.
///
/// # Errors
/// Returns an error if the image encoder fails.
pub fn encode_image(
    img: &RgbaImage,
    format: OutputFormat,
    export: &ExportOptions,
) -> Result<Vec<u8>> {
    match format {
        OutputFormat::Svg => Ok(render_svg(img, export.svg_shape, export.svg_skip_bg).into_bytes()),
        OutputFormat::Kitty => {
            let kitty_opts = KittyOptions {
                cols: export.view_cols.or(export.text_width),
                rows: export.view_rows,
                tmux_passthrough: KittyOptions::should_use_tmux_passthrough(),
                trailing_newline: true,
                no_cursor_move: false,
                image_id: None,
            };
            Ok(render_kitty(img, kitty_opts)?.into_bytes())
        }
        OutputFormat::Braille => Ok(render_braille(img, export.text_width).into_bytes()),
        OutputFormat::Ascii => Ok(render_ascii(img, export.text_width).into_bytes()),
        OutputFormat::Png if export.optimize => encode_optimized_png_bytes(img),
        OutputFormat::Png => encode_png_bytes(img),
        OutputFormat::Webp => crate::webp::encode_lossless(img),
        OutputFormat::Bmp => encode_bmp(img),
        OutputFormat::Tiff => encode_tiff(img),
        OutputFormat::Jpeg | OutputFormat::Gif => {
            if matches!(format, OutputFormat::Gif)
                && let Some(bytes) = crate::gif_lzw::encode_still(img)
            {
                return Ok(bytes);
            }
            let dyn_img = if matches!(format, OutputFormat::Jpeg) {
                DynamicImage::ImageRgba8(img.clone()).to_rgb8().into()
            } else {
                DynamicImage::ImageRgba8(img.clone())
            };
            let img_fmt = match format {
                OutputFormat::Jpeg => ImageFormat::Jpeg,
                OutputFormat::Gif => ImageFormat::Gif,
                OutputFormat::Png
                | OutputFormat::Webp
                | OutputFormat::Bmp
                | OutputFormat::Tiff
                | OutputFormat::Svg
                | OutputFormat::Kitty
                | OutputFormat::Braille
                | OutputFormat::Ascii => {
                    unreachable!("PNG, WebP, BMP, TIFF, and text/terminal formats handled above")
                }
            };
            let mut cursor = Cursor::new(Vec::new());
            dyn_img
                .write_to(&mut cursor, img_fmt)
                .with_context(|| format!("encoding {} output", format.id()))?;
            let bytes = cursor.into_inner();
            Ok(match format {
                OutputFormat::Jpeg if export.optimize => {
                    jpeg_with_optimal_huffman(&bytes).unwrap_or(bytes)
                }
                _ => bytes,
            })
        }
    }
}

/// Write encoded bytes to disk, creating parent directories automatically.
///
/// # Errors
/// Returns an error if directories cannot be created or the file cannot be written.
pub fn write_output_file(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating parent directory '{}'", parent.display()))?;
    }
    fs::write(path, bytes).with_context(|| format!("writing output file '{}'", path.display()))
}

/// Paint class of pixels no SVG run may cover: transparent holes and the background color.
const SVG_BARRIER: u32 = u32::MAX;

/// Longest dash period (one run plus one gap, in pixels) traced as a dashed stroke.
const SVG_MAX_DASH_PERIOD: u32 = 32;

/// Bytes a dash pattern must first save on paper before it earns its own `<path>`.
const SVG_DASH_PAYBACK: u32 = 64;

/// Dash periods per `<path>`. Skia-based renderers refuse to dash a path beyond one million.
const SVG_MAX_DASHES_PER_PATH: u32 = 50_000;

/// Render a dithered image as a compact, crisp SVG.
///
/// `Rect` mode strokes pixel runs as 1px-wide lines through the pixel centres. Opaque colors are
/// painted most frequent first and a run may pass under pixels a later color repaints, so runs
/// merge; each color picks horizontal or vertical runs, whichever is shorter, and evenly repeated
/// runs collapse into dashed strokes. Dot modes trace every dot as two filled semicircle arcs in
/// one `<path>` per color. Colors use the shortest hex form, so files stay lean for web and design
/// tooling.
#[must_use]
pub fn render_svg(img: &RgbaImage, shape: SvgShape, skip_bg: bool) -> String {
    let (w, h) = img.dimensions();
    let IndexedColors {
        colors,
        counts,
        indices,
    } = index_colors(img);

    let bg_color: Option<[u8; 3]> = {
        let mut by_rgb: BTreeMap<[u8; 3], u64> = BTreeMap::new();
        for (c, &count) in colors.iter().zip(&counts) {
            if c[3] > 0 {
                *by_rgb.entry([c[0], c[1], c[2]]).or_insert(0) += count;
            }
        }
        if colors.iter().any(|c| c[3] < 255) && !skip_bg {
            None
        } else {
            by_rgb
                .into_iter()
                .max_by_key(|&(_, c)| c)
                .map(|(rgb, _)| rgb)
        }
    };
    let skipped = |c: &[u8; 4]| c[3] == 0 || bg_color == Some([c[0], c[1], c[2]]);

    let body = match shape {
        SvgShape::Rect => svg_runs(w, h, &colors, &counts, indices, skipped),
        SvgShape::Circle | SvgShape::Dot => svg_dots(w, shape, &colors, &indices, skipped),
    };

    let mut out = String::with_capacity(body.len() + 256);
    let _ = write!(
        out,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {w} {h}\" width=\"{w}\" height=\"{h}\" shape-rendering=\"{}\">",
        if shape == SvgShape::Rect {
            "crispEdges"
        } else {
            "geometricPrecision"
        }
    );
    if let Some(bg) = bg_color
        && !skip_bg
    {
        let _ = write!(
            out,
            "<rect width=\"{w}\" height=\"{h}\" fill=\"{}\"/>",
            svg_hex(bg)
        );
    }
    out.push_str(&body);
    out.push_str("</svg>\n");
    out
}

/// Distinct RGBA colors of an image in first-seen order, their pixel counts, and each pixel's
/// index into that list.
struct IndexedColors {
    colors: Vec<[u8; 4]>,
    counts: Vec<u64>,
    indices: Vec<u32>,
}

fn index_colors(img: &RgbaImage) -> IndexedColors {
    const fn slot_of(key: u32, bits: u32) -> usize {
        (key.wrapping_mul(0x9E37_79B1) >> (32 - bits)) as usize
    }

    let (pixels, _) = img.as_raw().as_chunks::<4>();
    let mut colors: Vec<[u8; 4]> = Vec::new();
    let mut counts: Vec<u64> = Vec::new();
    let mut indices = Vec::with_capacity(pixels.len());
    let mut bits = 10;
    let mut slots: Vec<(u32, u32)> = vec![(0, 0); 1 << bits];
    let mut last = (0u32, u32::MAX);
    for &px in pixels {
        let key = u32::from_le_bytes(px);
        if last.0 != key || last.1 == u32::MAX {
            let mut slot = slot_of(key, bits);
            while slots[slot].1 != 0 && slots[slot].0 != key {
                slot = (slot + 1) & (slots.len() - 1);
            }
            if slots[slot].1 == 0 {
                colors.push(px);
                counts.push(0);
                slots[slot] = (key, colors.len() as u32);
                if colors.len() * 2 > slots.len() {
                    bits += 1;
                    slots = vec![(0, 0); 1 << bits];
                    for (c, index) in colors.iter().zip(1u32..) {
                        let key = u32::from_le_bytes(*c);
                        let mut slot = slot_of(key, bits);
                        while slots[slot].1 != 0 {
                            slot = (slot + 1) & (slots.len() - 1);
                        }
                        slots[slot] = (key, index);
                    }
                }
                last = (key, colors.len() as u32 - 1);
            } else {
                last = (key, slots[slot].1 - 1);
            }
        }
        counts[last.1 as usize] += 1;
        indices.push(last.1);
    }
    IndexedColors {
        colors,
        counts,
        indices,
    }
}

/// Stroke every pixel run of a `Rect` SVG, returning the `<g>` element (empty when nothing is drawn).
///
/// Opaque colors become layers ordered by descending pixel count; translucent colors follow and
/// keep their exact coverage. `skipped` marks colors that are never painted.
fn svg_runs(
    w: u32,
    h: u32,
    colors: &[[u8; 4]],
    counts: &[u64],
    mut codes: Vec<u32>,
    skipped: impl Fn(&[u8; 4]) -> bool,
) -> String {
    let painted = |opaque: bool| {
        let mut layer: Vec<usize> = (0..colors.len())
            .filter(|&i| !skipped(&colors[i]) && (colors[i][3] == 255) == opaque)
            .collect();
        layer.sort_by_key(|&i| {
            (
                std::cmp::Reverse(if opaque { counts[i] } else { 0 }),
                colors[i],
            )
        });
        layer
    };
    let mut layers = painted(true);
    let opaque = layers.len() as u32;
    layers.extend(painted(false));
    if layers.is_empty() {
        return String::new();
    }
    let mut code_of = vec![SVG_BARRIER; colors.len()];
    for (&i, code) in layers.iter().zip(0u32..) {
        code_of[i] = code;
    }
    for code in &mut codes {
        *code = code_of[*code as usize];
    }

    let (across, down) = rayon::join(
        || trace_runs(&codes, w as usize, opaque, layers.len(), false),
        || {
            let (w, h) = (w as usize, h as usize);
            let mut columns = vec![SVG_BARRIER; codes.len()];
            for y0 in (0..h).step_by(64) {
                for x0 in (0..w).step_by(64) {
                    for y in y0..h.min(y0 + 64) {
                        for x in x0..w.min(x0 + 64) {
                            columns[x * h + y] = codes[y * w + x];
                        }
                    }
                }
            }
            trace_runs(&columns, h, opaque, layers.len(), true)
        },
    );

    let mut out = String::from("<g fill=\"none\">");
    for ((across, down), &i) in across.iter().zip(&down).zip(&layers) {
        let rgba = colors[i];
        let mut stroke = format!("stroke=\"{}\"", svg_hex([rgba[0], rgba[1], rgba[2]]));
        if rgba[3] < 255 {
            let opacity = format!("{:.2}", f32::from(rgba[3]) / 255.0);
            let opacity = opacity.trim_end_matches('0').trim_end_matches('.');
            let opacity = match opacity.strip_prefix("0.") {
                Some(fraction) => format!(".{fraction}"),
                None => opacity.to_string(),
            };
            let _ = write!(stroke, " stroke-opacity=\"{opacity}\"");
        }
        if down.encoded_len() < across.encoded_len() {
            down.write(&mut out, &stroke);
        } else {
            across.write(&mut out, &stroke);
        }
    }
    out.push_str("</g>");
    out
}

/// Trace the stroked runs of every layer along consecutive lines of `length` pixels: rows, or
/// columns when `vertical`.
///
/// `codes` holds one paint class per pixel: a layer index, or [`SVG_BARRIER`]. Layers below
/// `opaque` may run under pixels of any later opaque layer; the rest cover their own pixels only.
fn trace_runs(
    codes: &[u32],
    length: usize,
    opaque: u32,
    layers: usize,
    vertical: bool,
) -> Vec<RunLayer> {
    let mut out: Vec<RunLayer> = (0..layers).map(|_| RunLayer::default()).collect();
    let mut pending: Vec<Vec<(u32, u32)>> = vec![Vec::new(); layers];
    let mut touched: Vec<u32> = Vec::new();
    let mut stack: Vec<(u32, u32, u32)> = Vec::new();
    for (row, line) in codes.chunks(length.max(1)).zip(0u32..) {
        let mut close = |(code, start, end): (u32, u32, u32)| {
            let runs = &mut pending[code as usize];
            if runs.is_empty() {
                touched.push(code);
            }
            runs.push((start, end));
        };
        let mut previous = SVG_BARRIER;
        for (&code, end) in row.iter().zip(1u32..) {
            if code == previous {
                if let Some(top) = stack.last_mut() {
                    top.2 = end;
                }
                continue;
            }
            previous = code;
            while let Some(&top) = stack.last()
                && (top.0 > code || (code >= opaque && top.0 != code))
            {
                close(top);
                stack.pop();
            }
            match stack.last_mut() {
                Some(top) if top.0 == code => top.2 = end,
                _ if code != SVG_BARRIER => stack.push((code, end - 1, end)),
                _ => {}
            }
        }
        while let Some(top) = stack.pop() {
            close(top);
        }
        for code in touched.drain(..) {
            let runs = &mut pending[code as usize];
            out[code as usize].push_line(vertical, line, runs);
            runs.clear();
        }
    }
    out
}

/// The stroked paths of one color: plain runs plus one dashed path set per repeated run pattern.
#[derive(Default)]
struct RunLayer {
    plain: RunPath,
    dashes: Vec<DashRuns>,
}

/// Runs that repeat `on` pixels painted, `off` pixels skipped, stroked with a matching dash array.
struct DashRuns {
    on: u32,
    off: u32,
    saved: u32,
    periods: u32,
    path: RunPath,
    full: Vec<String>,
}

impl RunLayer {
    /// Add one line's runs (`start..end` pixel spans in ascending order).
    fn push_line(&mut self, vertical: bool, line: u32, runs: &[(u32, u32)]) {
        let mut i = 0;
        while i < runs.len() {
            let (start, end) = runs[i];
            let on = end - start;
            let off = runs.get(i + 1).map_or(u32::MAX, |next| next.0 - end);
            let mut j = i;
            if off <= SVG_MAX_DASH_PERIOD.saturating_sub(on) {
                while let Some(&(s, e)) = runs.get(j + 1)
                    && s - runs[j].1 == off
                    && e - s == on
                {
                    j += 1;
                }
            }
            let repeats = (j - i) as u32;
            if repeats == 0 || !self.push_dashed(vertical, line, start, on, off, repeats) {
                for &(start, end) in &runs[i..=j] {
                    self.plain.push(vertical, line, start, end - start);
                }
            }
            i = j + 1;
        }
    }

    /// Stroke `repeats + 1` evenly spaced runs as one dashed line. Returns false while the pattern
    /// has not yet proven it saves more than its `<path>` costs; the caller then strokes plain runs.
    fn push_dashed(
        &mut self,
        vertical: bool,
        line: u32,
        start: u32,
        on: u32,
        off: u32,
        repeats: u32,
    ) -> bool {
        let index = self
            .dashes
            .iter()
            .position(|d| (d.on, d.off) == (on, off))
            .unwrap_or_else(|| {
                self.dashes.push(DashRuns {
                    on,
                    off,
                    saved: 0,
                    periods: 0,
                    path: RunPath::default(),
                    full: Vec::new(),
                });
                self.dashes.len() - 1
            });
        let dash = &mut self.dashes[index];
        if dash.saved < SVG_DASH_PAYBACK {
            let per_run = 4 + decimal_len(i64::from(off)) + decimal_len(i64::from(on));
            dash.saved += (repeats * per_run as u32).saturating_sub(2);
            return false;
        }
        if dash.periods + repeats + 1 > SVG_MAX_DASHES_PER_PATH && dash.periods > 0 {
            dash.full.push(std::mem::take(&mut dash.path).d);
            dash.periods = 0;
        }
        dash.periods += repeats + 1;
        dash.path
            .push(vertical, line, start, on + repeats * (on + off));
        true
    }

    /// Bytes [`Self::write`] emits, close enough to compare the two run directions.
    fn encoded_len(&self) -> usize {
        let dashed = self.dashes.iter().flat_map(|dash| {
            dash.full
                .iter()
                .chain([&dash.path.d])
                .filter(|d| !d.is_empty())
                .map(|d| d.len() + 40)
        });
        self.plain.d.len() + dashed.sum::<usize>()
    }

    /// Append this color's `<path>` elements, wrapped in a `<g>` when there are several.
    fn write(&self, out: &mut String, stroke: &str) {
        let mut paths: Vec<(String, &str)> = Vec::new();
        if !self.plain.d.is_empty() {
            paths.push((String::new(), &self.plain.d));
        }
        for dash in &self.dashes {
            let array = if dash.on == dash.off {
                format!(" stroke-dasharray=\"{}\"", dash.on)
            } else {
                format!(" stroke-dasharray=\"{} {}\"", dash.on, dash.off)
            };
            for d in dash.full.iter().chain([&dash.path.d]) {
                if !d.is_empty() {
                    paths.push((array.clone(), d));
                }
            }
        }
        let grouped = paths.len() > 1;
        if grouped {
            let _ = write!(out, "<g {stroke}>");
        }
        for (array, d) in paths {
            out.push_str("<path");
            if !grouped {
                out.push(' ');
                out.push_str(stroke);
            }
            out.push_str(&array);
            out.push_str(" d=\"");
            out.push_str(d);
            out.push_str("\"/>");
        }
        if grouped {
            out.push_str("</g>");
        }
    }
}

/// Path data for 1px-wide stroked runs, with the pen position after the last run.
#[derive(Default)]
struct RunPath {
    d: String,
    along: u32,
    line: u32,
}

impl RunPath {
    /// Append a run of `length` pixels starting `start` pixels into row or column `line`, moving
    /// there with whichever of an absolute or a relative move is shorter.
    fn push(&mut self, vertical: bool, line: u32, start: u32, length: u32) {
        let d_along = i64::from(start) - i64::from(self.along);
        let d_line = i64::from(line) - i64::from(self.line);
        let absolute = 4
            + decimal_len(i64::from(start))
            + if line == 0 {
                0
            } else {
                decimal_len(i64::from(line))
            };
        let relative =
            1 + decimal_len(d_along) + decimal_len(d_line) + usize::from(!vertical || d_along >= 0);
        let d = &mut self.d;
        if d.is_empty() || absolute < relative {
            let centre = |d: &mut String| {
                if line > 0 {
                    push_decimal(d, i64::from(line));
                }
                d.push_str(".5");
            };
            d.push('M');
            if vertical {
                centre(d);
                d.push(' ');
                push_decimal(d, i64::from(start));
            } else {
                push_decimal(d, i64::from(start));
                d.push(' ');
                centre(d);
            }
        } else {
            let (dx, dy) = if vertical {
                (d_line, d_along)
            } else {
                (d_along, d_line)
            };
            d.push('m');
            push_decimal(d, dx);
            if dy >= 0 {
                d.push(' ');
            }
            push_decimal(d, dy);
        }
        d.push(if vertical { 'v' } else { 'h' });
        push_decimal(d, i64::from(length));
        self.along = start + length;
        self.line = line;
    }
}

/// Trace every dot of a `Circle` or `Dot` SVG as two filled semicircle arcs, one `<path>` per
/// color (alpha is ignored, as dots are always opaque).
fn svg_dots(
    w: u32,
    shape: SvgShape,
    colors: &[[u8; 4]],
    indices: &[u32],
    skipped: impl Fn(&[u8; 4]) -> bool,
) -> String {
    let (left, arcs) = if shape == SvgShape::Circle {
        (".02", "a.48.48 0 10.96 0 .48.48 0 10-.96 0")
    } else {
        (".12", "a.38.38 0 10.76 0 .38.38 0 10-.76 0")
    };
    let mut by_rgb: BTreeMap<[u8; 3], usize> = BTreeMap::new();
    for c in colors.iter().filter(|c| !skipped(c)) {
        let next = by_rgb.len();
        by_rgb.entry([c[0], c[1], c[2]]).or_insert(next);
    }
    let path_of: Vec<usize> = colors
        .iter()
        .map(|c| {
            let path = by_rgb.get(&[c[0], c[1], c[2]]).filter(|_| !skipped(c));
            path.copied().unwrap_or(usize::MAX)
        })
        .collect();
    let mut dots: Vec<(String, u32, u32)> = vec![Default::default(); by_rgb.len()];
    for (row, y) in indices.chunks(w.max(1) as usize).zip(0u32..) {
        for (&index, x) in row.iter().zip(0u32..) {
            let Some((d, last_x, last_y)) = dots.get_mut(path_of[index as usize]) else {
                continue;
            };
            if d.is_empty() {
                d.push('M');
                push_decimal(d, i64::from(x));
                d.push_str(left);
                d.push(' ');
                if y > 0 {
                    push_decimal(d, i64::from(y));
                }
                d.push_str(".5");
            } else {
                d.push('m');
                push_decimal(d, i64::from(x) - i64::from(*last_x));
                d.push(' ');
                push_decimal(d, i64::from(y - *last_y));
            }
            d.push_str(arcs);
            (*last_x, *last_y) = (x, y);
        }
    }
    let mut out = String::with_capacity(dots.iter().map(|(d, _, _)| d.len() + 32).sum());
    for (rgb, i) in by_rgb {
        let _ = write!(out, "<path fill=\"{}\" d=\"{}\"/>", svg_hex(rgb), dots[i].0);
    }
    out
}

/// Characters [`push_decimal`] writes for `n`.
fn decimal_len(n: i64) -> usize {
    let digits = n.unsigned_abs().checked_ilog10().map_or(1, |log| log + 1);
    usize::from(n < 0) + digits as usize
}

/// Append `n` in decimal.
fn push_decimal(out: &mut String, n: i64) {
    let mut digits = [0u8; 20];
    let mut at = digits.len();
    let mut rest = n.unsigned_abs();
    loop {
        at -= 1;
        digits[at] = b'0' + (rest % 10) as u8;
        rest /= 10;
        if rest == 0 {
            break;
        }
    }
    if n < 0 {
        out.push('-');
    }
    for &digit in &digits[at..] {
        out.push(char::from(digit));
    }
}

/// Format a color as the shortest SVG hex literal, `#rgb` when every channel repeats its nibble.
fn svg_hex([r, g, b]: [u8; 3]) -> String {
    if [r, g, b].iter().all(|c| c >> 4 == c & 0xf) {
        format!("#{:x}{:x}{:x}", r & 0xf, g & 0xf, b & 0xf)
    } else {
        Rgb::new(r, g, b).to_hex()
    }
}

/// Render an image as high-resolution Unicode Braille characters (`U+2800..U+28FF`, 2x4 subpixels).
#[must_use]
pub fn render_braille(img: &RgbaImage, max_cols: Option<u32>) -> String {
    // Standard Braille dot bit mapping for (dx, dy) in 2x4 cell.
    const DOT_BITS: [[u8; 4]; 2] = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]];

    let target_cols = max_cols.unwrap_or(60).clamp(4, 240);
    let target_w = target_cols * 2;
    let src = if img.width() > target_w {
        let ratio = target_w as f32 / img.width() as f32;
        let target_h = ((img.height() as f32) * ratio).round().max(4.0) as u32;
        image::imageops::resize(
            img,
            target_w,
            target_h,
            image::imageops::FilterType::Nearest,
        )
    } else {
        img.clone()
    };

    // Adaptive luminance midpoint so both dark-on-light and light-on-dark palettes render clearly.
    let mut min_l = 1.0f32;
    let mut max_l = 0.0f32;
    for px in src.pixels() {
        if px[3] >= 128 {
            let l = Rgb::new(px[0], px[1], px[2]).lightness();
            min_l = min_l.min(l);
            max_l = max_l.max(l);
        }
    }
    let mid = f32::midpoint(min_l, max_l);

    let w = src.width();
    let h = src.height();
    let mut out = String::new();

    for cy in (0..h).step_by(4) {
        for cx in (0..w).step_by(2) {
            let mut mask = 0u8;
            for dx in 0..2u32 {
                for dy in 0..4u32 {
                    let x = cx + dx;
                    let y = cy + dy;
                    if x < w && y < h {
                        let px = *src.get_pixel(x, y);
                        if px[3] >= 128 && Rgb::new(px[0], px[1], px[2]).lightness() > mid {
                            mask |= DOT_BITS[dx as usize][dy as usize];
                        }
                    }
                }
            }
            let ch = char::from_u32(0x2800 + u32::from(mask)).unwrap_or(' ');
            out.push(ch);
        }
        out.push('\n');
    }

    out
}

/// Render an image as classic density-ramp ASCII text art.
#[must_use]
pub fn render_ascii(img: &RgbaImage, max_cols: Option<u32>) -> String {
    const RAMP: &[u8] = b" .:-=+*#%@";
    let target_w = max_cols.unwrap_or(80).clamp(8, 240);
    // Account for ~2:1 terminal character cell aspect ratio.
    let ratio = target_w as f32 / img.width().max(1) as f32;
    let target_h = ((img.height() as f32) * ratio * 0.5).round().max(1.0) as u32;
    let resized = image::imageops::resize(
        img,
        target_w,
        target_h,
        image::imageops::FilterType::Nearest,
    );

    let mut out = String::with_capacity((target_w as usize + 1) * target_h as usize);
    for y in 0..resized.height() {
        for x in 0..resized.width() {
            let px = *resized.get_pixel(x, y);
            if px[3] < 64 {
                out.push(' ');
            } else {
                let l = Rgb::new(px[0], px[1], px[2]).lightness().clamp(0.0, 1.0);
                let idx = ((l * (RAMP.len() - 1) as f32).round() as usize).min(RAMP.len() - 1);
                out.push(char::from(RAMP[idx]));
            }
        }
        out.push('\n');
    }
    out
}

/// Render an image as chunked Kitty graphics protocol escape sequences (PNG `f=100`, direct `t=d`).
///
/// Works natively in Kitty, Ghostty, `WezTerm`, Konsole, and other terminals supporting the Kitty
/// graphics protocol. Set `opts.tmux_passthrough` to wrap chunks in tmux DCS passthrough.
///
/// # Errors
/// Returns an error if lossless PNG encoding fails.
pub fn render_kitty(img: &RgbaImage, opts: KittyOptions) -> Result<String> {
    let png_bytes =
        encode_fast_png_bytes(img).context("encoding PNG payload for Kitty graphics protocol")?;
    Ok(encode_kitty_png(&png_bytes, opts))
}

/// Wrap pre-encoded PNG bytes into chunked Kitty graphics protocol APC escape sequences.
#[must_use]
pub fn encode_kitty_png(png_bytes: &[u8], opts: KittyOptions) -> String {
    let encoded = base64_encode(png_bytes);
    let total_len = encoded.len();
    let chunk_count = total_len.div_ceil(KITTY_CHUNK_SIZE).max(1);

    let mut out = String::with_capacity(total_len + chunk_count * 48);

    for (idx, chunk) in encoded.as_bytes().chunks(KITTY_CHUNK_SIZE).enumerate() {
        let is_first = idx == 0;
        let is_last = idx + 1 == chunk_count;
        let more = u8::from(!is_last);
        let chunk_str = std::str::from_utf8(chunk).unwrap_or("");

        if opts.tmux_passthrough {
            out.push_str("\x1bPtmux;\x1b\x1b_G");
        } else {
            out.push_str("\x1b_G");
        }

        if is_first {
            // a=T: transmit and display, f=100: PNG, t=d: direct payload, q=2: quiet (no acks).
            out.push_str("a=T,f=100,t=d,q=2");
            if let Some(id) = opts.image_id
                && id > 0
            {
                let _ = write!(out, ",i={id},p=1");
            }
            if opts.no_cursor_move {
                out.push_str(",C=1");
            }
            if let Some(cols) = opts.cols
                && cols > 0
            {
                let _ = write!(out, ",c={cols}");
            }
            if let Some(rows) = opts.rows
                && rows > 0
            {
                let _ = write!(out, ",r={rows}");
            }
            let _ = write!(out, ",m={more};{chunk_str}");
        } else {
            let _ = write!(out, "m={more};{chunk_str}");
        }

        if opts.tmux_passthrough {
            out.push_str("\x1b\x1b\\\x1b\\");
        } else {
            out.push_str("\x1b\\");
        }
    }

    if opts.trailing_newline {
        out.push('\n');
    }
    out
}

/// Encode an `RgbaImage` as the smallest lossless PNG layout that holds it exactly (for saved files).
///
/// See [`PngLayout`] for how the layout is chosen.
///
/// # Errors
/// Returns an error if PNG encoding fails.
pub fn encode_png_bytes(img: &RgbaImage) -> Result<Vec<u8>> {
    let layout = PngLayout::for_images([img]);
    if !matches!(layout, PngLayout::Indexed(_)) {
        return encode_png(img, png::Compression::High);
    }
    let scanlines = layout.unfiltered_scanlines(img);
    let mut out = layout.png_header(img.width(), img.height());
    for part in crate::deflate::zlib_compress(
        &scanlines,
        scanlines.len() / img.height().max(1) as usize,
        Effort::DEFAULT,
    )
    .chunks(i32::MAX as usize)
    {
        write_chunk(&mut out, *b"IDAT", part);
    }
    write_chunk(&mut out, *b"IEND", &[]);
    Ok(out)
}

/// Like [`encode_png_bytes`], but spends far more CPU for the smallest possible file: the palette
/// order and PNG filter strategy are searched, and the image data is compressed with Zopfli.
///
/// # Errors
/// Returns an error if PNG encoding fails.
pub fn encode_optimized_png_bytes(img: &RgbaImage) -> Result<Vec<u8>> {
    crate::png_optimize::recompress(&encode_png(img, png::Compression::NoCompression)?)
}

/// Fast lossless PNG encoding tailored for live interactive Kitty graphics previews.
///
/// Uses the same compact layout as [`encode_png_bytes`] with much cheaper DEFLATE settings.
///
/// # Errors
/// Returns an error if PNG encoding fails.
pub fn encode_fast_png_bytes(img: &RgbaImage) -> Result<Vec<u8>> {
    encode_png(img, png::Compression::Fast)
}

/// Encode `img` as a lossless WebP, the same bytes [`encode_image`] writes for
/// [`OutputFormat::Webp`].
///
/// # Errors
/// Returns an error if the image is larger than 16384x16384 pixels or encoding fails.
pub fn encode_webp_bytes(img: &RgbaImage) -> Result<Vec<u8>> {
    crate::webp::encode_lossless(img)
}

/// Encode `img` as a single-frame GIF with an exact color table, the same bytes [`encode_image`]
/// writes for [`OutputFormat::Gif`] when the image has at most 256 colors. Returns `None` when it
/// has more colors or is larger than GIF allows.
#[must_use]
pub fn encode_gif_bytes(img: &RgbaImage) -> Option<Vec<u8>> {
    crate::gif_lzw::encode_still(img)
}

fn encode_png(img: &RgbaImage, compression: png::Compression) -> Result<Vec<u8>> {
    let layout = PngLayout::for_images([img]);
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, img.width(), img.height());
        layout.configure(&mut encoder, compression);
        let mut writer = encoder.write_header().context("writing PNG header")?;
        writer
            .write_image_data(&layout.pixel_data(img))
            .context("writing PNG pixel data")?;
    }
    Ok(out)
}

/// Encode `img` as the most compact exact BMP: a 1-, 4-, or 8-bit palette when it is opaque with at
/// most 256 colors, else 24-bit RGB when opaque, else 32-bit BGRA.
fn encode_bmp(img: &RgbaImage) -> Result<Vec<u8>> {
    let layout = PngLayout::for_images([img]);
    if let PngLayout::Indexed(palette) = &layout
        && palette.iter().all(|c| c[3] == 255)
    {
        return Ok(encode_indexed_bmp(img, palette));
    }
    let (pixels, _) = img.as_raw().as_chunks::<4>();
    let (data, color) = if matches!(layout, PngLayout::Rgb) {
        let rgb: Vec<u8> = pixels.iter().flat_map(|px| [px[0], px[1], px[2]]).collect();
        (rgb, image::ExtendedColorType::Rgb8)
    } else {
        (img.as_raw().clone(), image::ExtendedColorType::Rgba8)
    };
    let mut out = Vec::new();
    image::codecs::bmp::BmpEncoder::new(&mut out)
        .encode(&data, img.width(), img.height(), color)
        .context("encoding bmp output")?;
    Ok(out)
}

/// Write a bottom-up palettized BMP (`BITMAPINFOHEADER`) for an opaque image with at most 256
/// colors: uncompressed at the smallest bit depth BMP supports for `palette` (1, 4, or 8 bits per
/// pixel), or run-length encoded (`BI_RLE4` up to 16 colors, else `BI_RLE8`) when that is smaller.
///
/// A black-then-white palette is never run-length encoded: Pillow reads it as a 1-bit image and
/// then fails on RLE data.
fn encode_indexed_bmp(img: &RgbaImage, palette: &[[u8; 4]]) -> Vec<u8> {
    const HEADERS_SIZE: usize = 14 + 40;
    let width = img.width() as usize;
    let indices = palette_indices(img, palette);
    let rows = || indices.chunks(width.max(1)).rev();

    let depth: usize = match palette.len() {
        0..=2 => 1,
        3..=16 => 4,
        _ => 8,
    };
    let per_byte = 8 / depth;
    let row_len = width.div_ceil(per_byte).next_multiple_of(4);
    let mut raw = vec![0u8; row_len * img.height() as usize];
    for (row, out) in rows().zip(raw.chunks_mut(row_len.max(1))) {
        for (x, &index) in row.iter().enumerate() {
            out[x / per_byte] |= index << (8 - depth * (x % per_byte + 1));
        }
    }
    let nibbles = palette.len() <= 16;
    let black_white =
        palette.len() == 2 && palette[0][..3] == [0; 3] && palette[1][..3] == [255; 3];
    let rle = bmp_rle(rows(), nibbles);
    let (depth, compression, data) = if rle.len() < raw.len() && !black_white {
        (
            if nibbles { 4 } else { 8 },
            if nibbles { 2 } else { 1 },
            rle,
        )
    } else {
        (depth, 0, raw)
    };

    let pixel_offset = HEADERS_SIZE + palette.len() * 4;
    let mut out = Vec::with_capacity(pixel_offset + data.len());
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&((pixel_offset + data.len()) as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(pixel_offset as u32).to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&img.width().to_le_bytes());
    out.extend_from_slice(&img.height().to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&(depth as u16).to_le_bytes());
    out.extend_from_slice(&(compression as u32).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&[0; 8]);
    out.extend_from_slice(&(palette.len() as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    for c in palette {
        out.extend_from_slice(&[c[2], c[1], c[0], 0]);
    }
    out.extend_from_slice(&data);
    out
}

/// BMP run-length encoding of rows of palette indices: `BI_RLE4` when `nibbles` (indices below 16),
/// else `BI_RLE8`.
///
/// A run repeats one index in RLE8 and alternates two indices in RLE4, up to 255 pixels for two
/// bytes; other pixels go into absolute-mode literals. A run is taken when it is long enough to
/// beat the literal bytes it replaces, counting the literal header it costs when it splits one.
/// RLE4 literals always hold an even number of pixels, because Pillow's decoder drops the last
/// pixel of an odd one.
fn bmp_rle<'a>(rows: impl Iterator<Item = &'a [u8]>, nibbles: bool) -> Vec<u8> {
    fn push_literal(out: &mut Vec<u8>, pixels: &[u8], nibbles: bool) {
        for chunk in pixels.chunks(if nibbles { 254 } else { 255 }) {
            let absolute = match chunk.len() {
                n if nibbles && n >= 4 => n & !1,
                n if !nibbles && n >= 3 => n,
                _ => 0,
            };
            if absolute > 0 {
                out.extend_from_slice(&[0, absolute as u8]);
                if nibbles {
                    let (pairs, _) = chunk[..absolute].as_chunks::<2>();
                    out.extend(pairs.iter().map(|&[a, b]| a << 4 | b));
                } else {
                    out.extend_from_slice(&chunk[..absolute]);
                }
                if out.len() % 2 == 1 {
                    out.push(0);
                }
            }
            for run in chunk[absolute..].chunks(if nibbles { 2 } else { 1 }) {
                let second = if nibbles {
                    run.get(1).map_or(0, |&b| b)
                } else {
                    0
                };
                out.extend_from_slice(&[
                    run.len() as u8,
                    run[0] << (if nibbles { 4 } else { 0 }) | second,
                ]);
            }
        }
    }

    let (split, fresh) = if nibbles { (8, 4) } else { (5, 2) };
    let mut out = Vec::new();
    for (y, row) in rows.enumerate() {
        if y > 0 {
            out.extend_from_slice(&[0, 0]);
        }
        let mut literal = 0;
        let mut i = 0;
        while i < row.len() {
            let extends = |run: usize| {
                let at = i + run;
                at < row.len()
                    && run < 255
                    && if nibbles {
                        run < 2 || row[at] == row[at - 2]
                    } else {
                        row[at] == row[i]
                    }
            };
            let mut run = 1;
            while extends(run) {
                run += 1;
            }
            if run >= if literal < i { split } else { fresh } {
                push_literal(&mut out, &row[literal..i], nibbles);
                let second = if nibbles && run > 1 { row[i + 1] } else { 0 };
                out.extend_from_slice(&[
                    run as u8,
                    if nibbles {
                        row[i] << 4 | second
                    } else {
                        row[i]
                    },
                ]);
                i += run;
                literal = i;
            } else {
                i += 1;
            }
        }
        push_literal(&mut out, &row[literal..], nibbles);
    }
    out.extend_from_slice(&[0, 1]);
    out
}

/// Raw image bytes per TIFF strip (rounded up to whole rows), the split the `tiff` crate's encoder
/// uses, so no reader needs one huge decompression buffer.
const TIFF_STRIP_BYTES: usize = 1_000_000;

/// Encode `img` as a Deflate-compressed baseline TIFF in the smallest exact layout that dith's own
/// decoder reads back: 1-bit bilevel when every pixel is opaque black or white, 8-bit gray when it
/// is opaque and colorless, else RGB when opaque, else RGBA with unassociated alpha.
///
/// Bilevel strips are compressed with the near-optimal DEFLATE encoder used for indexed PNGs; 8-bit
/// strips are three to four times larger and go through flate2 at its best level, which is several
/// times faster on them. Palette-color TIFFs would be smaller still, but the `tiff` decoder behind
/// `image` cannot read them.
fn encode_tiff(img: &RgbaImage) -> Result<Vec<u8>> {
    use std::io::Write as _;

    const SHORT: u16 = 3;
    const LONG: u16 = 4;
    const RATIONAL: u16 = 5;

    let (width, height) = img.dimensions();
    let (pixels, _) = img.as_raw().as_chunks::<4>();
    let opaque = pixels.iter().all(|px| px[3] == 255);
    let gray = opaque && pixels.iter().all(|px| px[0] == px[1] && px[1] == px[2]);
    let (bits, samples, data) = if gray && pixels.iter().all(|px| px[0] == 0 || px[0] == 255) {
        let row_bytes = (width as usize).div_ceil(8);
        let mut bilevel = vec![0u8; row_bytes * height as usize];
        for (row, out) in pixels
            .chunks(width.max(1) as usize)
            .zip(bilevel.chunks_mut(row_bytes.max(1)))
        {
            for (x, px) in row.iter().enumerate() {
                out[x / 8] |= (px[0] & 1) << (7 - x % 8);
            }
        }
        (1, 1, bilevel)
    } else if gray {
        (8, 1, pixels.iter().map(|px| px[0]).collect())
    } else if opaque {
        let rgb = pixels.iter().flat_map(|px| [px[0], px[1], px[2]]).collect();
        (8, 3, rgb)
    } else {
        (8, 4, img.as_raw().clone())
    };
    let row_bytes = data.len() / height.max(1) as usize;
    let rows_per_strip = TIFF_STRIP_BYTES
        .div_ceil(row_bytes.max(1))
        .min(height.max(1) as usize);

    let mut out = b"II*\0\0\0\0\0".to_vec();
    let mut offsets = Vec::new();
    let mut lengths = Vec::new();
    for strip in data.chunks((rows_per_strip * row_bytes).max(1)) {
        let compressed = if bits == 1 {
            crate::deflate::zlib_compress(strip, row_bytes, Effort::DEFAULT)
        } else {
            let mut zlib = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
            zlib.write_all(strip)?;
            zlib.finish()?
        };
        offsets.push(u32::try_from(out.len())?);
        lengths.push(u32::try_from(compressed.len())?);
        out.extend_from_slice(&compressed);
    }
    out.resize(out.len().next_multiple_of(2), 0);

    let mut tags: Vec<(u16, u16, Vec<u32>)> = vec![
        (256, LONG, vec![width]),
        (257, LONG, vec![height]),
        (258, SHORT, vec![bits; samples]),
        (259, SHORT, vec![8]),
        (262, SHORT, vec![if samples < 3 { 1 } else { 2 }]),
        (273, LONG, offsets),
        (277, SHORT, vec![samples as u32]),
        (278, LONG, vec![rows_per_strip as u32]),
        (279, LONG, lengths),
        (282, RATIONAL, vec![1, 1]),
        (283, RATIONAL, vec![1, 1]),
        (296, SHORT, vec![1]),
    ];
    if samples == 4 {
        tags.push((338, SHORT, vec![2]));
    }
    let ifd = out.len();
    out[4..8].copy_from_slice(&u32::try_from(ifd)?.to_le_bytes());
    let external = ifd + 2 + tags.len() * 12 + 4;
    let mut extra = Vec::new();
    out.extend_from_slice(&(tags.len() as u16).to_le_bytes());
    for (tag, kind, values) in tags {
        let bytes: Vec<u8> = values
            .iter()
            .flat_map(|&v| match kind {
                SHORT => (v as u16).to_le_bytes().to_vec(),
                _ => v.to_le_bytes().to_vec(),
            })
            .collect();
        let count = if kind == RATIONAL {
            values.len() / 2
        } else {
            values.len()
        };
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(&kind.to_le_bytes());
        out.extend_from_slice(&(count as u32).to_le_bytes());
        if bytes.len() <= 4 {
            let mut field = [0; 4];
            field[..bytes.len()].copy_from_slice(&bytes);
            out.extend_from_slice(&field);
        } else {
            out.extend_from_slice(&u32::try_from(external + extra.len())?.to_le_bytes());
            extra.extend_from_slice(&bytes);
        }
    }
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&extra);
    Ok(out)
}

/// Rewrite a baseline JPEG from the `image` encoder with Huffman tables built for its own symbol
/// statistics (ITU T.81 Annex K.2), as a single grayscale component when every chroma block is
/// zero (a colorless image). The quantized coefficients are untouched, so decoders reconstruct the
/// same pixels. Returns `None` for streams it does not handle (progressive, restart intervals,
/// subsampled chroma) or when the rewrite would not be smaller.
fn jpeg_with_optimal_huffman(jpeg: &[u8]) -> Option<Vec<u8>> {
    let mut kept: Vec<(u8, &[u8])> = Vec::new();
    let mut tables: [Option<JpegHuffman>; 8] = Default::default();
    let mut sof: Option<&[u8]> = None;
    let mut components: Vec<(u8, u8)> = Vec::new();
    let mut scan: Vec<(usize, usize, usize)> = Vec::new();
    if !jpeg.starts_with(&[0xFF, 0xD8]) || !jpeg.ends_with(&[0xFF, 0xD9]) {
        return None;
    }
    let mut at = 2;
    loop {
        let &[0xFF, marker, high, low] = jpeg.get(at..at + 4)? else {
            return None;
        };
        let payload = jpeg.get(at + 4..at + 2 + usize::from(u16::from_be_bytes([high, low])))?;
        at += 2 + payload.len() + 2;
        match marker {
            0xC4 => {
                let mut rest = payload;
                while let [class_id, counts @ ..] = rest {
                    let counts: [u8; 16] = counts.get(..16)?.try_into().ok()?;
                    let n = counts.iter().map(|&c| usize::from(c)).sum::<usize>();
                    let symbols = rest.get(17..17 + n)?.to_vec();
                    let (class, id) = (usize::from(class_id >> 4), usize::from(class_id & 15));
                    *tables.get_mut(id * 2 + class).filter(|_| class < 2)? =
                        Some(JpegHuffman { counts, symbols });
                    rest = &rest[17 + n..];
                }
            }
            0xC0 => {
                let (&[8, _, _, _, _, n], entries) = payload.split_first_chunk::<6>()? else {
                    return None;
                };
                components = entries
                    .as_chunks::<3>()
                    .0
                    .iter()
                    .map(|c| (c[1] == 0x11).then_some((c[0], c[2])))
                    .collect::<Option<_>>()?;
                (matches!(n, 1 | 3) && components.len() == usize::from(n)).then_some(())?;
                sof = Some(payload);
            }
            0xDA => {
                sof?;
                let (&n, rest) = payload.split_first()?;
                (usize::from(n) == components.len()
                    && rest.get(usize::from(n) * 2..)? == [0, 63, 0])
                .then_some(())?;
                for selector in rest[..usize::from(n) * 2].as_chunks::<2>().0 {
                    let component = components.iter().position(|c| c.0 == selector[0])?;
                    let (dc, ac) = (usize::from(selector[1] >> 4), usize::from(selector[1] & 15));
                    (dc < 4 && ac < 4).then_some(())?;
                    scan.push((component, dc * 2, ac * 2 + 1));
                }
                break;
            }
            0xDB | 0xE0..=0xEF | 0xFE => kept.push((marker, payload)),
            _ => return None,
        }
    }
    let sof = sof?;
    let width = usize::from(u16::from_be_bytes([sof[3], sof[4]]));
    let height = usize::from(u16::from_be_bytes([sof[1], sof[2]]));

    let mut reader = JpegBitReader {
        data: &jpeg[at..jpeg.len() - 2],
        ..JpegBitReader::default()
    };
    let fast: Vec<Vec<(u8, u8)>> = tables
        .iter()
        .map(|table| table.as_ref().map_or_else(Vec::new, JpegHuffman::lookahead))
        .collect();
    let mut tokens: Vec<(u8, u8, u16)> = Vec::with_capacity(jpeg.len() * 2);
    let mut chroma_zero = scan.len() == 3;
    for _ in 0..width.div_ceil(8) * height.div_ceil(8) {
        for (i, &(_, dc, ac)) in scan.iter().enumerate() {
            let first = tokens.len();
            let (symbol, extra) = reader.decode(tables[dc].as_ref()?, &fast[dc], false)?;
            (symbol <= 11).then_some(())?;
            tokens.push(((i << 4 | dc) as u8, symbol, extra));
            let mut k = 1;
            while k < 64 {
                let (symbol, extra) = reader.decode(tables[ac].as_ref()?, &fast[ac], true)?;
                tokens.push(((i << 4 | ac) as u8, symbol, extra));
                match (symbol >> 4, symbol & 15) {
                    (0, 0) => break,
                    (15, 0) => k += 16,
                    (_, 0) => return None,
                    (run, _) => k += usize::from(run) + 1,
                }
            }
            (k <= 64).then_some(())?;
            chroma_zero &= i == 0 || tokens[first..].iter().all(|t| t.1 == 0);
        }
    }
    let gray = chroma_zero;
    if gray {
        tokens.retain(|token| token.0 >> 4 == 0);
    }
    let luma = scan[0];

    let mut freq = [[0u32; 256]; 8];
    for &(table, symbol, _) in &tokens {
        freq[usize::from(table & 15)][usize::from(symbol)] += 1;
    }
    let mut dht = Vec::new();
    let mut codes = [[(0u8, 0u16); 256]; 8];
    for (t, freq) in freq.iter().enumerate() {
        if freq.iter().all(|&f| f == 0) {
            continue;
        }
        let table = JpegHuffman::optimal(freq);
        dht.push(((t & 1) << 4 | t >> 1) as u8);
        dht.extend_from_slice(&table.counts);
        dht.extend_from_slice(&table.symbols);
        codes[t] = table.codes();
    }

    let mut out = vec![0xFF, 0xD8];
    let segment = |out: &mut Vec<u8>, marker: u8, payload: &[u8]| {
        out.extend_from_slice(&[0xFF, marker]);
        out.extend_from_slice(&(payload.len() as u16 + 2).to_be_bytes());
        out.extend_from_slice(payload);
    };
    let (luma_id, luma_tq) = components[luma.0];
    for (marker, payload) in kept {
        let unused_table = marker == 0xDB && payload.len() == 65 && payload[0] != luma_tq;
        if !(gray && unused_table) {
            segment(&mut out, marker, payload);
        }
    }
    if gray {
        let mut sof = sof[..5].to_vec();
        sof.extend_from_slice(&[1, luma_id, 0x11, luma_tq]);
        segment(&mut out, 0xC0, &sof);
    } else {
        segment(&mut out, 0xC0, sof);
    }
    segment(&mut out, 0xC4, &dht);
    if gray {
        let selector = (((luma.1 / 2) << 4) | (luma.2 / 2)) as u8;
        segment(&mut out, 0xDA, &[1, luma_id, selector, 0, 63, 0]);
    } else {
        let mut sos = vec![scan.len() as u8];
        for &(component, dc, ac) in &scan {
            sos.extend_from_slice(&[components[component].0, (((dc / 2) << 4) | (ac / 2)) as u8]);
        }
        sos.extend_from_slice(&[0, 63, 0]);
        segment(&mut out, 0xDA, &sos);
    }
    let mut writer = JpegBitWriter {
        out,
        acc: 0,
        len: 0,
    };
    for (table, symbol, extra) in tokens {
        let (len, code) = codes[usize::from(table & 15)][usize::from(symbol)];
        writer.put(u32::from(code), u32::from(len));
        let extra_len = if table & 1 == 0 { symbol } else { symbol & 15 };
        writer.put(u32::from(extra), u32::from(extra_len));
    }
    let mut out = writer.finish();
    out.extend_from_slice(&[0xFF, 0xD9]);
    (out.len() < jpeg.len()).then_some(out)
}

/// A JPEG Huffman table: how many codes there are of each length 1..=16, and the symbols in code
/// order.
#[derive(Debug, Clone, Default)]
struct JpegHuffman {
    counts: [u8; 16],
    symbols: Vec<u8>,
}

impl JpegHuffman {
    /// The optimal length-limited table for symbol frequencies `freq`, built as ITU T.81 Annex K.2
    /// (and libjpeg) do: Huffman merging with a reserved all-ones code, then lengths above 16
    /// folded back.
    fn optimal(freq: &[u32; 256]) -> Self {
        let mut freq: Vec<u64> = freq.iter().map(|&f| u64::from(f)).chain([1]).collect();
        let mut size = [0usize; 257];
        let mut next = [usize::MAX; 257];
        let least = |freq: &[u64], skip: usize| {
            (0..257)
                .filter(|&i| i != skip && freq[i] > 0)
                .fold(None, |best: Option<usize>, i| match best {
                    Some(b) if freq[b] < freq[i] => Some(b),
                    _ => Some(i),
                })
        };
        while let Some(mut v1) = least(&freq, usize::MAX)
            && let Some(mut v2) = least(&freq, v1)
        {
            freq[v1] += freq[v2];
            freq[v2] = 0;
            size[v1] += 1;
            while next[v1] != usize::MAX {
                v1 = next[v1];
                size[v1] += 1;
            }
            next[v1] = v2;
            size[v2] += 1;
            while next[v2] != usize::MAX {
                v2 = next[v2];
                size[v2] += 1;
            }
        }
        let mut bits = [0usize; 258];
        for &s in size.iter().filter(|&&s| s > 0) {
            bits[s] += 1;
        }
        for i in (17..258).rev() {
            while bits[i] > 0 {
                let mut j = i - 2;
                while bits[j] == 0 {
                    j -= 1;
                }
                bits[i] -= 2;
                bits[i - 1] += 1;
                bits[j + 1] += 2;
                bits[j] -= 1;
            }
        }
        let longest = (1..=16).rev().find(|&i| bits[i] > 0).unwrap_or(1);
        bits[longest] -= 1;
        let mut counts = [0u8; 16];
        for (count, &bits) in counts.iter_mut().zip(&bits[1..=16]) {
            *count = bits as u8;
        }
        let symbols = (1..258)
            .flat_map(|len| (0..256).filter(move |&s| size[s] == len))
            .map(|s| s as u8)
            .collect();
        Self { counts, symbols }
    }

    /// (code length, symbol) for every 9-bit prefix that starts with a code of at most 9 bits, and
    /// (0, 0) for the rest, so most symbols decode with one lookup.
    fn lookahead(&self) -> Vec<(u8, u8)> {
        let mut fast = vec![(0, 0); 512];
        let mut code = 0usize;
        let mut symbols = self.symbols.iter();
        for (len, &count) in (1u8..=9).zip(&self.counts) {
            for &symbol in symbols.by_ref().take(usize::from(count)) {
                let span = 1 << (9 - len);
                fast[code * span..(code + 1) * span].fill((len, symbol));
                code += 1;
            }
            code <<= 1;
        }
        fast
    }

    /// (code length, code) of every symbol, for encoding.
    fn codes(&self) -> [(u8, u16); 256] {
        let mut codes = [(0, 0); 256];
        let mut code = 0u16;
        let mut symbols = self.symbols.iter();
        for (len, &count) in (1u8..).zip(&self.counts) {
            for &symbol in symbols.by_ref().take(usize::from(count)) {
                codes[usize::from(symbol)] = (len, code);
                code += 1;
            }
            code <<= 1;
        }
        codes
    }
}

/// Reads bits from JPEG entropy-coded data, removing the `0xFF 0x00` byte stuffing.
#[derive(Default)]
struct JpegBitReader<'a> {
    data: &'a [u8],
    at: usize,
    acc: u64,
    len: u32,
    padding: u32,
}

impl JpegBitReader<'_> {
    fn refill(&mut self) {
        if self.len > 48 {
            return;
        }
        if let Some(&word) = self
            .data
            .get(self.at..)
            .and_then(|rest| rest.first_chunk::<8>())
        {
            let word = u64::from_be_bytes(word);
            let ones = !word;
            if ones.wrapping_sub(0x0101_0101_0101_0101) & !ones & 0x8080_8080_8080_8080 == 0 {
                let bytes = ((64 - self.len) / 8).min(7);
                self.acc = self.acc << (bytes * 8) | word >> (64 - bytes * 8);
                self.len += bytes * 8;
                self.at += bytes as usize;
                return;
            }
        }
        while self.len <= 48 {
            let byte = match self.data.get(self.at..) {
                Some([0xFF, 0x00, ..]) => {
                    self.at += 2;
                    0xFF
                }
                Some([0xFF, ..] | []) | None => {
                    self.padding += 8;
                    0
                }
                Some([byte, ..]) => {
                    self.at += 1;
                    *byte
                }
            };
            self.acc = self.acc << 8 | u64::from(byte);
            self.len += 8;
        }
    }

    /// The next Huffman-coded symbol of `table` (whose [`JpegHuffman::lookahead`] is `fast`) and the
    /// extra bits that follow it: as many as the symbol after a DC symbol, its low four bits after
    /// an AC one. `None` past the end of the data.
    fn decode(&mut self, table: &JpegHuffman, fast: &[(u8, u8)], ac: bool) -> Option<(u8, u16)> {
        self.refill();
        let peek = (self.acc >> (self.len - 16)) as u32 & 0xFFFF;
        let (len, symbol) = if let Some(&(len @ 1.., symbol)) = fast.get((peek >> 7) as usize) {
            (len, symbol)
        } else {
            let (mut first, mut index) = (0u32, 0usize);
            let mut found = None;
            for (len, &count) in (1u8..).zip(&table.counts) {
                let code = peek >> (16 - u32::from(len));
                if code < first + u32::from(count) {
                    found = Some((len, *table.symbols.get(index + (code - first) as usize)?));
                    break;
                }
                index += usize::from(count);
                first = (first + u32::from(count)) << 1;
            }
            found?
        };
        let extra = u32::from(if ac { symbol & 15 } else { symbol });
        let used = u32::from(len) + extra;
        (extra <= 16 && self.len - used >= self.padding).then_some(())?;
        self.len -= used;
        Some((
            symbol,
            ((self.acc >> self.len) as u32 & ((1 << extra) - 1)) as u16,
        ))
    }
}

/// Writes JPEG entropy-coded data, stuffing a zero byte after every `0xFF`.
struct JpegBitWriter {
    out: Vec<u8>,
    acc: u64,
    len: u32,
}

impl JpegBitWriter {
    fn put(&mut self, bits: u32, n: u32) {
        self.acc = self.acc << n | u64::from(bits);
        self.len += n;
        if self.len >= 32 {
            self.len -= 32;
            self.push_bytes(&((self.acc >> self.len) as u32).to_be_bytes());
        }
    }

    fn push_bytes(&mut self, bytes: &[u8]) {
        if !bytes.contains(&0xFF) {
            self.out.extend_from_slice(bytes);
            return;
        }
        for &byte in bytes {
            self.out.push(byte);
            if byte == 0xFF {
                self.out.push(0);
            }
        }
    }

    /// Pad the last byte with one bits and return the data.
    fn finish(mut self) -> Vec<u8> {
        let pad = (8 - self.len % 8) % 8;
        self.acc = self.acc << pad | ((1 << pad) - 1);
        self.len += pad;
        let tail = (self.acc << (64 - self.len)).to_be_bytes();
        self.push_bytes(&tail[..(self.len / 8) as usize]);
        self.out
    }
}

/// The most compact exact PNG pixel layout for a set of images (one still, or every animation frame).
///
/// Dithered output almost always uses a handful of colors, so it becomes an indexed PNG at the
/// smallest bit depth that fits the palette (1, 2, 4, or 8 bits per pixel) with a `tRNS` chunk for
/// any translucent entries. Images with more than 256 distinct colors fall back to 24-bit RGB when
/// fully opaque, else 32-bit RGBA.
#[derive(Debug)]
pub(crate) enum PngLayout {
    Indexed(Vec<[u8; 4]>),
    Rgb,
    Rgba,
}

impl PngLayout {
    pub(crate) fn for_images<'a>(images: impl IntoIterator<Item = &'a RgbaImage>) -> Self {
        let mut palette: Vec<[u8; 4]> = Vec::new();
        let mut seen = ColorIndex::default();
        let mut opaque = true;
        let mut indexable = true;
        for img in images {
            let (pixels, _) = img.as_raw().as_chunks::<4>();
            let mut last = None;
            for &px in pixels {
                if last == Some(px) {
                    continue;
                }
                last = Some(px);
                opaque &= px[3] == 255;
                if indexable && seen.get(px).is_none() {
                    indexable = palette.len() < 256;
                    if indexable {
                        seen.insert(px, palette.len() as u8);
                    }
                    palette.push(px);
                }
            }
            if !indexable && !opaque {
                break;
            }
        }
        if indexable && !palette.is_empty() {
            palette.sort_by_key(|px| px[3] == 255);
            Self::Indexed(palette)
        } else if opaque {
            Self::Rgb
        } else {
            Self::Rgba
        }
    }

    fn bit_depth(&self) -> u8 {
        match self {
            Self::Indexed(palette) => match palette.len() {
                0..=2 => 1,
                3..=4 => 2,
                5..=16 => 4,
                _ => 8,
            },
            Self::Rgb | Self::Rgba => 8,
        }
    }

    pub(crate) fn configure<W: std::io::Write>(
        &self,
        encoder: &mut png::Encoder<'_, W>,
        compression: png::Compression,
    ) {
        encoder.set_compression(compression);
        match self {
            Self::Indexed(palette) => {
                encoder.set_color(png::ColorType::Indexed);
                encoder.set_depth(match self.bit_depth() {
                    1 => png::BitDepth::One,
                    2 => png::BitDepth::Two,
                    4 => png::BitDepth::Four,
                    _ => png::BitDepth::Eight,
                });
                encoder.set_palette(
                    palette
                        .iter()
                        .flat_map(|c| [c[0], c[1], c[2]])
                        .collect::<Vec<_>>(),
                );
                let trns: Vec<u8> = palette
                    .iter()
                    .map(|c| c[3])
                    .take_while(|&a| a < 255)
                    .collect();
                if !trns.is_empty() {
                    encoder.set_trns(trns);
                }
                encoder.set_filter(png::Filter::NoFilter);
            }
            Self::Rgb => {
                encoder.set_color(png::ColorType::Rgb);
                encoder.set_depth(png::BitDepth::Eight);
            }
            Self::Rgba => {
                encoder.set_color(png::ColorType::Rgba);
                encoder.set_depth(png::BitDepth::Eight);
            }
        }
    }

    /// Raw PNG scanline data (before filtering) for `img`, whose colors must be covered by this layout.
    pub(crate) fn pixel_data(&self, img: &RgbaImage) -> Vec<u8> {
        self.scanlines(img, false)
    }

    /// PNG scanlines of `img` with the filter type byte 0 (None) in front of every row.
    pub(crate) fn unfiltered_scanlines(&self, img: &RgbaImage) -> Vec<u8> {
        self.scanlines(img, true)
    }

    fn scanlines(&self, img: &RgbaImage, filter_bytes: bool) -> Vec<u8> {
        let (pixels, _) = img.as_raw().as_chunks::<4>();
        let width = img.width() as usize;
        let lead = usize::from(filter_bytes);
        match self {
            Self::Indexed(palette) => {
                let indices = palette_indices(img, palette);
                let depth = usize::from(self.bit_depth());
                let per_byte = 8 / depth;
                let row_bytes = lead + width.div_ceil(per_byte);
                let mut out = vec![0u8; row_bytes * img.height() as usize];
                for (row, out_row) in indices.chunks(width.max(1)).zip(out.chunks_mut(row_bytes)) {
                    for (group, byte) in row.chunks(per_byte).zip(&mut out_row[lead..]) {
                        for (k, &index) in group.iter().enumerate() {
                            *byte |= index << (8 - depth * (k + 1));
                        }
                    }
                }
                out
            }
            Self::Rgb | Self::Rgba => {
                let channels = if matches!(self, Self::Rgb) { 3 } else { 4 };
                let mut out = Vec::with_capacity((lead + width * channels) * img.height() as usize);
                for row in pixels.chunks(width.max(1)) {
                    out.extend_from_slice(&[0][..lead]);
                    for px in row {
                        out.extend_from_slice(&px[..channels]);
                    }
                }
                out
            }
        }
    }

    /// The PNG signature, `IHDR` and, for indexed layouts, `PLTE` and `tRNS` chunks.
    pub(crate) fn png_header(&self, width: u32, height: u32) -> Vec<u8> {
        let mut out = crate::png_optimize::SIGNATURE.to_vec();
        let mut ihdr = [0u8; 13];
        ihdr[..4].copy_from_slice(&width.to_be_bytes());
        ihdr[4..8].copy_from_slice(&height.to_be_bytes());
        ihdr[8] = self.bit_depth();
        ihdr[9] = match self {
            Self::Indexed(_) => 3,
            Self::Rgb => 2,
            Self::Rgba => 6,
        };
        write_chunk(&mut out, *b"IHDR", &ihdr);
        if let Self::Indexed(palette) = self {
            let plte: Vec<u8> = palette.iter().flat_map(|c| [c[0], c[1], c[2]]).collect();
            write_chunk(&mut out, *b"PLTE", &plte);
            let trns: Vec<u8> = palette
                .iter()
                .map(|c| c[3])
                .take_while(|&a| a < 255)
                .collect();
            if !trns.is_empty() {
                write_chunk(&mut out, *b"tRNS", &trns);
            }
        }
        out
    }
}

/// Open-addressed map from RGBA colors to palette indices, for palettes of at most 256 colors.
#[derive(Debug)]
pub(crate) struct ColorIndex {
    slots: Vec<(u32, u16)>,
}

impl Default for ColorIndex {
    fn default() -> Self {
        Self {
            slots: vec![(0, Self::EMPTY); 1 << Self::BITS],
        }
    }
}

impl ColorIndex {
    const BITS: u32 = 10;
    const EMPTY: u16 = u16::MAX;

    pub(crate) fn from_palette(palette: &[[u8; 4]]) -> Self {
        let mut index = Self::default();
        for (i, &color) in palette.iter().enumerate().take(256) {
            index.insert(color, i as u8);
        }
        index
    }

    fn slot(&self, key: u32) -> usize {
        let mut slot = (key.wrapping_mul(0x9E37_79B1) >> (32 - Self::BITS)) as usize;
        while self.slots[slot].1 != Self::EMPTY && self.slots[slot].0 != key {
            slot = (slot + 1) & ((1 << Self::BITS) - 1);
        }
        slot
    }

    pub(crate) fn get(&self, color: [u8; 4]) -> Option<u8> {
        let key = u32::from_le_bytes(color);
        let (_, value) = self.slots[self.slot(key)];
        (value != Self::EMPTY).then_some(value as u8)
    }

    pub(crate) fn insert(&mut self, color: [u8; 4], index: u8) {
        let key = u32::from_le_bytes(color);
        let slot = self.slot(key);
        self.slots[slot] = (key, u16::from(index));
    }
}

/// The palette index of every pixel of `img`, whose colors must all be in `palette`.
pub(crate) fn palette_indices(img: &RgbaImage, palette: &[[u8; 4]]) -> Vec<u8> {
    let lookup = ColorIndex::from_palette(palette);
    let (pixels, _) = img.as_raw().as_chunks::<4>();
    let mut last = (palette.first().copied().unwrap_or_default(), 0u8);
    pixels
        .iter()
        .map(|&px| {
            if px != last.0 {
                last = (px, lookup.get(px).unwrap_or(0));
            }
            last.1
        })
        .collect()
}

/// Standard RFC 4648 Base64 encoder (zero extra dependencies).
fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    let (chunks, rem) = input.as_chunks::<3>();
    for chunk in chunks {
        let n = (u32::from(chunk[0]) << 16) | (u32::from(chunk[1]) << 8) | u32::from(chunk[2]);
        out.push(char::from(TABLE[((n >> 18) & 0x3F) as usize]));
        out.push(char::from(TABLE[((n >> 12) & 0x3F) as usize]));
        out.push(char::from(TABLE[((n >> 6) & 0x3F) as usize]));
        out.push(char::from(TABLE[(n & 0x3F) as usize]));
    }
    match rem.len() {
        1 => {
            let n = u32::from(rem[0]) << 16;
            out.push(char::from(TABLE[((n >> 18) & 0x3F) as usize]));
            out.push(char::from(TABLE[((n >> 12) & 0x3F) as usize]));
            out.push('=');
            out.push('=');
        }
        2 => {
            let n = (u32::from(rem[0]) << 16) | (u32::from(rem[1]) << 8);
            out.push(char::from(TABLE[((n >> 18) & 0x3F) as usize]));
            out.push(char::from(TABLE[((n >> 12) & 0x3F) as usize]));
            out.push(char::from(TABLE[((n >> 6) & 0x3F) as usize]));
            out.push('=');
        }
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_histogram_counts_and_orders_like_a_sorted_map() {
        let mut state = 0x1234_5678_u32;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state
        };
        let img = RgbaImage::from_fn(333, 211, |_, _| {
            let v = next();
            // A handful of colors in runs, some stray ones, and transparent pixels.
            let [a, b, c, d] = v.to_le_bytes();
            let rgb = if a < 200 {
                [b & 0xC0, 0x40, c & 0x80]
            } else {
                [b, c, d]
            };
            image::Rgba([rgb[0], rgb[1], rgb[2], if a % 7 == 0 { 0 } else { 255 }])
        });
        let mut reference: BTreeMap<[u8; 3], u64> = BTreeMap::new();
        for px in img.pixels().filter(|px| px[3] != 0) {
            *reference.entry([px[0], px[1], px[2]]).or_insert(0) += 1;
        }
        let mut expected: Vec<(String, u64)> = reference
            .into_iter()
            .map(|(rgb, n)| (Rgb::new(rgb[0], rgb[1], rgb[2]).to_hex(), n))
            .collect();
        expected.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let got: Vec<(String, u64)> = color_histogram(&img)
            .into_iter()
            .map(|row| (row.hex, row.pixels))
            .collect();
        assert_eq!(got, expected);
    }

    use image::Rgba;

    #[test]
    fn base64_matches_rfc4648_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn kitty_graphics_chunks_and_headers_follow_spec() {
        let img = RgbaImage::from_pixel(8, 8, Rgba([20, 40, 180, 255]));
        let single = render_kitty(
            &img,
            KittyOptions {
                cols: Some(40),
                rows: None,
                tmux_passthrough: false,
                trailing_newline: true,
                no_cursor_move: false,
                image_id: None,
            },
        )
        .unwrap();
        assert!(single.starts_with("\x1b_Ga=T,f=100,t=d,q=2,c=40,m=0;"));
        assert!(single.ends_with("\x1b\\\n"));

        // Verify stable image_id framing for flicker-free animation replacement.
        let atomic = encode_kitty_png(
            b"hi",
            KittyOptions {
                image_id: Some(1),
                no_cursor_move: true,
                trailing_newline: false,
                ..KittyOptions::default()
            },
        );
        assert!(atomic.starts_with("\x1b_Ga=T,f=100,t=d,q=2,i=1,p=1,C=1,m=0;"));

        // Verify multi-chunk framing (>4096 base64 chars).
        let big_payload = vec![0xABu8; 4000];
        let multi = encode_kitty_png(&big_payload, KittyOptions::default());
        assert!(multi.starts_with("\x1b_Ga=T,f=100,t=d,q=2,m=1;"));
        assert!(multi.contains("\x1b\\\x1b_Gm=0;"));
        assert!(multi.ends_with("\x1b\\\n"));

        // Verify tmux DCS passthrough doubles ESC bytes.
        let tmux = encode_kitty_png(
            b"hi",
            KittyOptions {
                cols: None,
                rows: None,
                tmux_passthrough: true,
                trailing_newline: true,
                no_cursor_move: false,
                image_id: None,
            },
        );
        assert!(tmux.starts_with("\x1bPtmux;\x1b\x1b_Ga=T,f=100,t=d,q=2,m=0;"));
        assert!(tmux.ends_with("\x1b\x1b\\\x1b\\\n"));
    }

    #[test]
    fn svg_export_merges_horizontal_runs() {
        let mut img = RgbaImage::from_pixel(8, 2, Rgba([255, 255, 255, 255]));
        for x in 2..6 {
            img.put_pixel(x, 0, Rgba([0, 0, 0, 255]));
        }
        for x in 1..3 {
            img.put_pixel(x, 1, Rgba([0, 0, 0, 255]));
        }
        let svg = render_svg(&img, SvgShape::Rect, false);
        assert!(
            svg.contains("<rect width=\"8\" height=\"2\" fill=\"#fff\"/>"),
            "{svg}"
        );
        assert!(
            svg.contains("<path stroke=\"#000\" d=\"M2 .5h4m-5 1h2\"/>"),
            "expected stroked RLE runs: {svg}"
        );

        img.put_pixel(7, 1, Rgba([0x12, 0x34, 0x56, 128]));
        let svg = render_svg(&img, SvgShape::Rect, false);
        assert!(
            svg.contains("<path stroke=\"#123456\" stroke-opacity=\".5\" d=\"M7 1.5h1\"/>"),
            "expected translucent run: {svg}"
        );
    }

    #[test]
    fn svg_dot_export_traces_each_dot_as_filled_arcs() {
        let mut img = RgbaImage::from_pixel(4, 3, Rgba([255, 255, 255, 255]));
        img.put_pixel(1, 0, Rgba([0xaa, 0xbb, 0xcc, 255]));
        img.put_pixel(3, 0, Rgba([0xaa, 0xbb, 0xcc, 255]));
        img.put_pixel(0, 2, Rgba([0xaa, 0xbb, 0xcc, 255]));
        let arcs = "a.48.48 0 10.96 0 .48.48 0 10-.96 0";
        let svg = render_svg(&img, SvgShape::Circle, true);
        assert!(!svg.contains("<rect"), "{svg}");
        assert!(
            svg.contains(&format!(
                "<path fill=\"#abc\" d=\"M1.02 .5{arcs}m2 0{arcs}m-3 2{arcs}\"/>"
            )),
            "expected one arc pair per dot: {svg}"
        );
        let dot = render_svg(&img, SvgShape::Dot, true);
        assert!(
            dot.contains("d=\"M1.12 .5a.38.38 0 10.76 0 .38.38 0 10-.76 0m2 0a.38"),
            "{dot}"
        );
    }

    /// A stroke color and, when translucent, its opacity.
    type SvgPaint = ([u8; 3], Option<f32>);

    /// Every paint a `Rect` SVG from [`render_svg`] applies to each pixel, in document order: the
    /// stroke color and its opacity when translucent. Also returns the background rect color.
    fn paint_svg_runs(svg: &str, w: u32, h: u32) -> (Option<[u8; 3]>, Vec<Vec<SvgPaint>>) {
        fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
            let start = tag.find(&format!(" {name}=\""))? + name.len() + 3;
            Some(&tag[start..start + tag[start..].find('"')?])
        }
        fn rgb(hex: &str) -> [u8; 3] {
            let digits: Vec<u8> = hex[1..]
                .chars()
                .map(|c| c.to_digit(16).unwrap() as u8)
                .collect();
            match digits.len() {
                3 => [digits[0] * 17, digits[1] * 17, digits[2] * 17],
                _ => [0, 2, 4].map(|i| digits[i] * 16 + digits[i + 1]),
            }
        }
        let mut painted = vec![Vec::new(); (w * h) as usize];
        let mut rect = None;
        let mut groups: Vec<(Option<&str>, Option<&str>)> = vec![(None, None)];
        for tag in svg.split('<').skip(1) {
            let (stroke, opacity) = *groups.last().unwrap();
            let stroke = attr(tag, "stroke").or(stroke);
            let opacity = attr(tag, "stroke-opacity").or(opacity);
            if tag.starts_with("rect ") {
                rect = Some(rgb(attr(tag, "fill").unwrap()));
            } else if tag.starts_with("g ") {
                groups.push((stroke, opacity));
            } else if tag.starts_with("/g") {
                groups.pop();
            } else if tag.starts_with("path ") {
                let paint = (
                    rgb(stroke.unwrap()),
                    opacity.map(|o| o.parse::<f32>().unwrap()),
                );
                let dash: Option<Vec<u32>> = attr(tag, "stroke-dasharray")
                    .map(|a| a.split(' ').map(|n| n.parse().unwrap()).collect());
                let d = attr(tag, "d").unwrap();
                let mut numbers: Vec<f64> = Vec::new();
                let (mut x, mut y) = (0.0, 0.0);
                let mut command = ' ';
                let mut chars = d.char_indices().peekable();
                while let Some((at, c)) = chars.next() {
                    if c.is_ascii_alphabetic() {
                        command = c;
                        continue;
                    }
                    if c == ' ' {
                        continue;
                    }
                    let mut end = at + 1;
                    let mut dot = c == '.';
                    while let Some(&(next_at, next)) = chars.peek()
                        && (next.is_ascii_digit() || (next == '.' && !dot))
                    {
                        dot |= next == '.';
                        end = next_at + 1;
                        chars.next();
                    }
                    numbers.push(d[at..end].parse().unwrap());
                    match (command, numbers.as_slice()) {
                        ('M', &[nx, ny]) => (x, y) = (nx, ny),
                        ('m', &[dx, dy]) => (x, y) = (x + dx, y + dy),
                        ('h' | 'v', &[length]) => {
                            let vertical = command == 'v';
                            assert_eq!(
                                (x.fract(), y.fract()),
                                if vertical { (0.5, 0.0) } else { (0.0, 0.5) }
                            );
                            for step in 0..length as u32 {
                                let on = dash.as_ref().is_none_or(|dash| {
                                    let (on, off) = (dash[0], *dash.last().unwrap());
                                    step % (on + off) < on
                                });
                                let (cx, cy) = if vertical {
                                    (x as u32, y as u32 + step)
                                } else {
                                    (x as u32 + step, y as u32)
                                };
                                if on {
                                    assert!(cx < w && cy < h, "{cx},{cy} outside {w}x{h}");
                                    painted[(cy * w + cx) as usize].push(paint);
                                }
                            }
                            if vertical {
                                y += length;
                            } else {
                                x += length;
                            }
                        }
                        _ => continue,
                    }
                    numbers.clear();
                }
            }
        }
        (rect, painted)
    }

    #[test]
    fn svg_rect_strokes_reproduce_every_pixel() {
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut random = move |n: u32| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((state >> 33) % u64::from(n)) as u32
        };
        let ink = |i: u32| Rgba([(i * 53) as u8, (255 - i * 31) as u8, (i * 97) as u8, 255]);
        let noise = RgbaImage::from_fn(61, 37, |_, _| {
            let i = random(9);
            ink(if i > 4 { 0 } else { i })
        });
        let ordered = RgbaImage::from_fn(96, 64, |x, y| {
            let i = [0, 2, 3, 1][((x % 4) ^ (y % 2)) as usize];
            ink(if (x / 24 + y / 16) % 3 == 0 { 0 } else { i })
        });
        let columns = RgbaImage::from_fn(48, 70, |x, y| ink(u32::from(x % 3 == 0 && y % 23 != 5)));
        let mut holes = noise.clone();
        for (x, y, px) in holes.enumerate_pixels_mut() {
            match random(7) {
                0 => px[3] = 0,
                1 if px[0] != 0 => px[3] = 90 + (x + y) as u8 % 3,
                _ => {}
            }
        }

        for (name, img) in [
            ("noise", noise),
            ("ordered", ordered),
            ("columns", columns),
            ("holes", holes),
        ] {
            for skip_bg in [false, true] {
                let svg = render_svg(&img, SvgShape::Rect, skip_bg);
                let (rect, painted) = paint_svg_runs(&svg, img.width(), img.height());
                let bg = (skip_bg || img.pixels().all(|px| px[3] == 255)).then_some([0, 255, 0]);
                assert_eq!(rect, bg.filter(|_| !skip_bg), "{name}: {svg}");
                for ((x, y, px), paints) in img.enumerate_pixels().zip(&painted) {
                    let rgb = [px[0], px[1], px[2]];
                    let at = format!("{name} skip_bg={skip_bg} at {x},{y}");
                    match px[3] {
                        0 => assert!(paints.is_empty(), "{at}"),
                        255 if skip_bg && Some(rgb) == bg => assert!(paints.is_empty(), "{at}"),
                        255 => assert_eq!(
                            paints.last().map_or(rect, |p| p.1.is_none().then_some(p.0)),
                            Some(rgb),
                            "{at}"
                        ),
                        alpha => {
                            assert_eq!(paints.len(), 1, "{at}");
                            assert_eq!(paints[0].0, rgb, "{at}");
                            assert!(
                                (paints[0].1.unwrap() - f32::from(alpha) / 255.0).abs() < 0.006,
                                "{at}"
                            );
                        }
                    }
                }
                if matches!(name, "noise" | "ordered") {
                    assert!(
                        painted.iter().any(|p| p.len() > 1),
                        "{name}: no run passes under a later color"
                    );
                }
                match name {
                    "ordered" => assert!(svg.contains("stroke-dasharray"), "{svg}"),
                    "columns" => assert!(!svg.contains("h1") && svg.contains("v5"), "{svg}"),
                    _ => {}
                }
            }
        }
    }

    #[test]
    fn fast_png_indexed_and_rgba_round_trip_identically() {
        let mut dithered = RgbaImage::from_pixel(16, 12, Rgba([244, 240, 228, 255]));
        for y in 0..12 {
            for x in 0..16 {
                if (x + y) % 2 == 0 {
                    dithered.put_pixel(x, y, Rgba([26, 43, 140, 255]));
                }
            }
        }
        let indexed_bytes = encode_fast_png_bytes(&dithered).unwrap();
        let decoded = image::load_from_memory(&indexed_bytes).unwrap().to_rgba8();
        assert_eq!(decoded, dithered);

        let mut translucent = dithered;
        translucent.put_pixel(0, 0, Rgba([10, 20, 30, 128]));
        let rgba_bytes = encode_fast_png_bytes(&translucent).unwrap();
        let decoded_rgba = image::load_from_memory(&rgba_bytes).unwrap().to_rgba8();
        assert_eq!(decoded_rgba, translucent);
    }

    #[test]
    fn saved_png_uses_smallest_exact_layout() {
        let decode = |img: &RgbaImage| {
            let bytes = encode_png_bytes(img).unwrap();
            let reader = png::Decoder::new(Cursor::new(bytes.clone()))
                .read_info()
                .unwrap();
            let info = reader.info();
            let layout = (info.color_type, info.bit_depth, info.trns.is_some());
            (layout, image::load_from_memory(&bytes).unwrap().to_rgba8())
        };

        let mut two_tone = RgbaImage::from_pixel(13, 5, Rgba([244, 240, 228, 255]));
        two_tone.put_pixel(12, 4, Rgba([26, 43, 140, 255]));
        let (layout, decoded) = decode(&two_tone);
        assert_eq!(layout, (png::ColorType::Indexed, png::BitDepth::One, false));
        assert_eq!(decoded, two_tone);

        let mut cutout = two_tone.clone();
        cutout.put_pixel(0, 0, Rgba([0, 0, 0, 0]));
        cutout.put_pixel(1, 0, Rgba([9, 9, 9, 100]));
        let (layout, decoded) = decode(&cutout);
        assert_eq!(layout, (png::ColorType::Indexed, png::BitDepth::Two, true));
        assert_eq!(decoded, cutout);

        let photo = RgbaImage::from_fn(32, 32, |x, y| Rgba([(x * 8) as u8, (y * 8) as u8, 7, 255]));
        let (layout, decoded) = decode(&photo);
        assert_eq!(layout, (png::ColorType::Rgb, png::BitDepth::Eight, false));
        assert_eq!(decoded, photo);
    }

    fn layout_samples() -> Vec<RgbaImage> {
        let mut two_tone = RgbaImage::from_pixel(13, 5, Rgba([244, 240, 228, 255]));
        two_tone.put_pixel(12, 4, Rgba([26, 43, 140, 255]));
        let mut cutout = two_tone.clone();
        cutout.put_pixel(0, 0, Rgba([0, 0, 0, 0]));
        cutout.put_pixel(1, 0, Rgba([9, 9, 9, 100]));
        let tones = |n: u32| {
            RgbaImage::from_fn(37, 23, move |x, y| {
                let i = ((x * 7 + y * 3) % n) as u8;
                Rgba([i * 11, 255 - i * 5, i, 255])
            })
        };
        let photo = RgbaImage::from_fn(32, 32, |x, y| Rgba([(x * 8) as u8, (y * 8) as u8, 7, 255]));
        let translucent =
            RgbaImage::from_fn(32, 32, |x, y| Rgba([(x * 8) as u8, 90, 7, (y * 8) as u8]));
        let gray = RgbaImage::from_fn(30, 20, |x, y| {
            let v = if (x + y) % 3 == 0 { 0 } else { 200 };
            Rgba([v, v, v, 255])
        });
        vec![
            two_tone,
            cutout,
            tones(5),
            tones(20),
            photo,
            translucent,
            gray,
        ]
    }

    #[test]
    fn optimized_png_round_trips_and_never_grows() {
        for img in layout_samples() {
            let optimized = encode_optimized_png_bytes(&img).unwrap();
            assert_eq!(image::load_from_memory(&optimized).unwrap().to_rgba8(), img);
            assert!(optimized.len() <= encode_png_bytes(&img).unwrap().len());
        }
    }

    #[test]
    fn bmp_uses_smallest_exact_layout() {
        let bits_per_pixel: Vec<u16> = layout_samples()
            .iter()
            .map(|img| {
                let bytes =
                    encode_image(img, OutputFormat::Bmp, &ExportOptions::default()).unwrap();
                assert_eq!(image::load_from_memory(&bytes).unwrap().to_rgba8(), *img);
                u16::from_le_bytes([bytes[28], bytes[29]])
            })
            .collect();
        assert_eq!(bits_per_pixel, [1, 32, 4, 8, 24, 32, 1]);
    }

    #[test]
    fn bmp_run_length_encodes_when_smaller() {
        let layout = |img: &RgbaImage| {
            let bytes = encode_image(img, OutputFormat::Bmp, &ExportOptions::default()).unwrap();
            assert_eq!(image::load_from_memory(&bytes).unwrap().to_rgba8(), *img);
            let bits = u16::from_le_bytes([bytes[28], bytes[29]]);
            (bits, u32::from_le_bytes(bytes[30..34].try_into().unwrap()))
        };
        let ink = |i: u32| Rgba([(i * 7) as u8, (200 - i * 3) as u8, (i * 5) as u8, 255]);
        let hash = |x: u32, y: u32| (x.wrapping_mul(2_654_435_761) ^ y.wrapping_mul(40_503)) >> 7;
        let mixed = |colors: u32| {
            RgbaImage::from_fn(613, 9, move |x, y| {
                ink(match x {
                    0..300 => y % colors,
                    300..500 => (x + y) % 2 * (colors - 1),
                    _ => hash(x, y) % colors,
                })
            })
        };
        let sparse = RgbaImage::from_fn(613, 9, |x, y| ink(u32::from(x > 400 && (x + y) % 2 == 0)));
        let noise = RgbaImage::from_fn(613, 9, |x, y| ink(hash(x, y) % 16));
        let layouts: Vec<(u16, u32)> = [mixed(4), mixed(40), sparse, noise]
            .iter()
            .map(layout)
            .collect();
        assert_eq!(layouts, [(4, 2), (8, 1), (4, 2), (4, 0)]);
    }

    #[test]
    fn jpeg_huffman_rewrite_decodes_to_the_same_pixels() {
        let photo = RgbaImage::from_fn(37, 21, |x, y| {
            Rgba([(x * 7) as u8, (y * 11) as u8, ((x ^ y) * 5) as u8, 255])
        });
        let gray = RgbaImage::from_fn(29, 17, |x, y| {
            let v = if (x * 3 + y * 5) % 7 < 3 { 0 } else { 255 };
            Rgba([v, v, v, 255])
        });
        let tiny = RgbaImage::from_pixel(1, 1, Rgba([200, 30, 90, 255]));
        for (img, components) in [(photo, 3), (gray, 1), (tiny, 3)] {
            let mut baseline = Cursor::new(Vec::new());
            DynamicImage::ImageRgba8(img.clone())
                .to_rgb8()
                .write_to(&mut baseline, ImageFormat::Jpeg)
                .unwrap();
            let baseline = baseline.into_inner();
            let optimize = ExportOptions {
                optimize: true,
                ..ExportOptions::default()
            };
            assert_eq!(
                encode_image(&img, OutputFormat::Jpeg, &ExportOptions::default()).unwrap(),
                baseline
            );
            let bytes = encode_image(&img, OutputFormat::Jpeg, &optimize).unwrap();
            assert!(bytes.len() < baseline.len());
            assert_eq!(
                image::load_from_memory(&bytes).unwrap().to_rgba8(),
                image::load_from_memory(&baseline).unwrap().to_rgba8()
            );
            let sof = bytes.windows(2).position(|w| w == [0xFF, 0xC0]).unwrap();
            assert_eq!(bytes[sof + 9], components);
        }
    }

    #[test]
    fn jpeg_optimal_huffman_codes_stay_within_16_bits() {
        let mut freq = [0u32; 256];
        let (mut a, mut b) = (1u32, 1u32);
        for f in freq.iter_mut().take(40) {
            *f = a;
            (a, b) = (b, a + b);
        }
        let table = JpegHuffman::optimal(&freq);
        assert_eq!(table.symbols.len(), 40);
        let kraft: u32 = (0..16)
            .map(|i| u32::from(table.counts[i]) << (15 - i))
            .sum();
        assert!(kraft < 1 << 16);
        let codes = table.codes();
        let used: Vec<(u8, u16)> = table
            .symbols
            .iter()
            .map(|&s| codes[usize::from(s)])
            .collect();
        for (i, &(len_a, code_a)) in used.iter().enumerate() {
            for &(len_b, code_b) in &used[i + 1..] {
                let len = len_a.min(len_b);
                assert_ne!(code_a >> (len_a - len), code_b >> (len_b - len));
            }
        }
    }

    #[test]
    fn tiff_is_deflate_compressed_and_exact() {
        for img in layout_samples() {
            let bytes = encode_image(&img, OutputFormat::Tiff, &ExportOptions::default()).unwrap();
            assert_eq!(image::load_from_memory(&bytes).unwrap().to_rgba8(), img);
            assert!(bytes.len() < img.as_raw().len());
        }
    }

    #[test]
    fn tiff_uses_smallest_layout_dith_reads_back() {
        let mut samples = layout_samples();
        samples.push(RgbaImage::from_fn(13, 7, |x, y| {
            let v = if (x * 3 + y) % 4 == 0 { 255 } else { 0 };
            Rgba([v, v, v, 255])
        }));
        let layouts: Vec<(u16, u16)> = samples
            .iter()
            .map(|img| {
                let bytes =
                    encode_image(img, OutputFormat::Tiff, &ExportOptions::default()).unwrap();
                assert_eq!(image::load_from_memory(&bytes).unwrap().to_rgba8(), *img);
                let u16_at = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]);
                let u32_at = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
                let ifd = u32_at(4) as usize;
                let entry = |tag: u16| {
                    (0..usize::from(u16_at(ifd)))
                        .map(|i| ifd + 2 + i * 12)
                        .find(|&at| u16_at(at) == tag)
                        .unwrap()
                };
                let samples = u16_at(entry(277) + 8);
                let bits = entry(258) + 8;
                let bits = if samples > 2 {
                    u16_at(u32_at(bits) as usize)
                } else {
                    u16_at(bits)
                };
                (samples, bits)
            })
            .collect();
        assert_eq!(
            layouts,
            [
                (3, 8),
                (4, 8),
                (3, 8),
                (3, 8),
                (3, 8),
                (4, 8),
                (1, 8),
                (1, 1)
            ]
        );
    }

    #[test]
    fn braille_and_ascii_render_non_empty() {
        let mut img = RgbaImage::from_pixel(16, 16, Rgba([0, 0, 0, 255]));
        for y in 0..8 {
            for x in 0..16 {
                img.put_pixel(x, y, Rgba([255, 255, 255, 255]));
            }
        }
        let braille = render_braille(&img, Some(8));
        assert!(braille.contains('\u{28ff}'));
        let ascii = render_ascii(&img, Some(16));
        assert!(ascii.contains('@'));
    }
}
