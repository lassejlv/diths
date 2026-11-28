//! The dith-wasm calls on plain Rust types, with no JavaScript involved.
//!
//! The `#[wasm_bindgen]` layer in `lib.rs` only converts arguments and results. Keeping the work
//! here lets the native harness (`examples/native.rs`) run exactly the code the `.wasm` runs, so a
//! pixel difference between the two comes from the target and not from glue code.

use std::fmt::Display;

use dith_core::dither::Algorithm;
use dith_core::generate::{GenerateOptions, Pattern, generate_pattern};
use dith_core::image::RgbaImage;
use dith_core::output::{ExportOptions, encode_png_bytes, render_svg};
use dith_core::palette::list_builtin_palettes;
use dith_core::preset::{all_presets, preset_by_name};
use dith_core::recipe::{DitherRecipe, PipelineOutput, RecipeError, run_pipeline_owned};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

/// Largest canvas side `generate` draws, so a typo cannot ask for gigabytes.
pub const MAX_CANVAS: u32 = 16_384;

/// Output formats this build encodes, in the order `formats()` reports them.
pub const FORMATS: &[&str] = &[
    "png",
    "svg",
    #[cfg(feature = "gif")]
    "gif",
    #[cfg(feature = "webp")]
    "webp",
];

/// The CLI's JSON error envelope: a machine code, a message and a recovery hint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Error {
    pub code: &'static str,
    pub message: String,
    pub suggestion: String,
    #[serde(rename = "validValues", skip_serializing_if = "Option::is_none")]
    pub valid_values: Option<Vec<String>>,
}

impl From<RecipeError> for Error {
    fn from(err: RecipeError) -> Self {
        Self {
            code: err.code(),
            message: err.to_string(),
            suggestion: err.suggestion(),
            valid_values: err.valid_values(),
        }
    }
}

impl Error {
    fn invalid(field: &'static str, reason: String) -> Self {
        RecipeError::Validation { field, reason }.into()
    }

    fn unsupported(field: &'static str, reason: String, valid: &[&str]) -> Self {
        Self {
            valid_values: Some(valid.iter().map(ToString::to_string).collect()),
            ..Self::invalid(field, reason)
        }
    }

    fn runtime(err: &dyn Display) -> Self {
        Self {
            code: "RUNTIME_ERROR",
            message: format!("{err:#}"),
            suggestion: "Check the pixel buffer and options; the same recipe should fail the same way in the dith CLI."
                .to_string(),
            valid_values: None,
        }
    }
}

/// Lay `patch` over `base`: objects merge key by key, anything else replaces.
fn merge(base: &mut Value, patch: Value) {
    match (base, patch) {
        (Value::Object(base), Value::Object(patch)) => {
            for (key, value) in patch {
                match base.get_mut(&key) {
                    Some(slot) => merge(slot, value),
                    None => {
                        base.insert(key, value);
                    }
                }
            }
        }
        (slot, value) => *slot = value,
    }
}

/// Parse `patch` over the JSON form of `defaults`, so callers may leave out any field.
fn parse_over<T: Serialize + DeserializeOwned>(defaults: &T, patch: &str) -> Result<T, String> {
    let patch: Value = serde_json::from_str(patch).map_err(|e| e.to_string())?;
    if !patch.is_object() {
        return Err("expected a JSON object".to_string());
    }
    let mut value = serde_json::to_value(defaults).map_err(|e| e.to_string())?;
    merge(&mut value, patch);
    serde_json::from_value(value).map_err(|e| e.to_string())
}

/// Resolve a recipe argument: a built-in preset name, or recipe JSON as `dith --recipe` reads it.
///
/// Recipe JSON may leave out any field, nested ones included; missing fields take the CLI's
/// defaults. A complete recipe (for example from `dith apply --emit-recipe`) renders exactly as the
/// CLI renders it. The recipe's `preset` field is a label, as in the CLI; use [`preset`] to
/// start from a preset.
///
/// # Errors
/// Returns `UNKNOWN_PRESET` or `INVALID_RECIPE_JSON`.
pub fn recipe(spec: &str) -> Result<DitherRecipe, Error> {
    let spec = spec.trim();
    if !spec.starts_with('{') {
        return preset(spec, false);
    }
    parse_over(&DitherRecipe::default(), spec).map_err(|e| RecipeError::InvalidRecipeJson(e).into())
}

/// The recipe of a built-in preset, as `dith --preset` seeds it. With `procedural`, photo
/// preparation (auto-levels, contrast, sharpen) is dropped, as `dith generate --preset` does.
///
/// # Errors
/// Returns `UNKNOWN_PRESET` with the valid preset ids.
pub fn preset(name: &str, procedural: bool) -> Result<DitherRecipe, Error> {
    let key = name.trim().to_ascii_lowercase().replace(' ', "-");
    let mut recipe = preset_by_name(&key).map_err(RecipeError::from)?.to_recipe();
    if procedural {
        recipe.strip_photo_prep();
    }
    Ok(recipe)
}

/// Wrap a caller's RGBA buffer as an image without copying it.
///
/// # Errors
/// Returns `INVALID_ARGUMENT` when the buffer length is not `width * height * 4`.
pub fn image(rgba: Vec<u8>, width: u32, height: u32) -> Result<RgbaImage, Error> {
    let expected = u64::from(width) * u64::from(height) * 4;
    if width == 0 || height == 0 || rgba.len() as u64 != expected {
        return Err(Error::invalid(
            "rgba",
            format!(
                "expected {width}x{height}x4 = {expected} bytes of RGBA, got {}",
                rgba.len()
            ),
        ));
    }
    RgbaImage::from_raw(width, height, rgba)
        .ok_or_else(|| Error::invalid("rgba", "buffer does not match its size".to_string()))
}

/// Run the dith pipeline (preprocess, palette, dither, finalize) on `img`.
///
/// # Errors
/// Returns the recipe's validation or palette error.
pub fn dither(img: RgbaImage, recipe: &DitherRecipe) -> Result<PipelineOutput, Error> {
    Ok(run_pipeline_owned(img, recipe)?)
}

/// Parse generator options over the CLI's defaults and check the canvas size.
///
/// # Errors
/// Returns `INVALID_RECIPE_JSON` or `INVALID_ARGUMENT`.
pub fn generate_options(spec: &str) -> Result<GenerateOptions, Error> {
    let defaults = GenerateOptions {
        width: 640,
        height: 480,
        ..GenerateOptions::default()
    };
    let opts = parse_over(&defaults, spec).map_err(RecipeError::InvalidRecipeJson)?;
    for (field, value) in [("width", opts.width), ("height", opts.height)] {
        if !(1..=MAX_CANVAS).contains(&value) {
            return Err(Error::invalid(
                field,
                format!("must be in 1..={MAX_CANVAS} (got {value})"),
            ));
        }
    }
    opts.inflate.validate()?;
    Ok(opts)
}

/// Encoded formats [`decode`] reads: PNG, JPEG, GIF and WebP with the `decode` feature, none
/// without it.
pub const DECODERS: &[&str] = &[
    #[cfg(feature = "decode")]
    "png",
    #[cfg(feature = "decode")]
    "jpeg",
    #[cfg(feature = "decode")]
    "gif",
    #[cfg(feature = "decode")]
    "webp",
];

fn undecodable(reason: &str) -> Error {
    Error {
        code: "INVALID_ARGUMENT",
        message: format!("invalid argument 'input': {reason}"),
        suggestion: if DECODERS.is_empty() {
            "Decode the image on the host (createImageBitmap, canvas) and pass RGBA pixels, or use the dith-wasm build with the `decode` feature.".to_string()
        } else {
            "Pass PNG, JPEG, GIF or WebP bytes, or decode the image yourself and pass RGBA pixels."
                .to_string()
        },
        valid_values: Some(DECODERS.iter().map(ToString::to_string).collect()),
    }
}

/// Decode PNG, JPEG, GIF (first frame) or WebP bytes to RGBA with the decoders the dith CLI
/// reads files with, so the pixels match `dith apply` on the same file. Only these four codecs
/// are linked, which keeps the module small.
///
/// # Errors
/// Returns `INVALID_ARGUMENT` for another or an unrecognised format, or `RUNTIME_ERROR` for
/// corrupt data.
#[cfg(feature = "decode")]
pub fn decode(bytes: &[u8]) -> Result<RgbaImage, Error> {
    use dith_core::image::codecs::{
        gif::GifDecoder, jpeg::JpegDecoder, png::PngDecoder, webp::WebPDecoder,
    };
    use dith_core::image::{DynamicImage, ImageFormat};
    use std::io::Cursor;

    let cursor = || Cursor::new(bytes);
    let decoded = match dith_core::image::guess_format(bytes) {
        Ok(ImageFormat::Png) => PngDecoder::new(cursor()).and_then(DynamicImage::from_decoder),
        Ok(ImageFormat::Jpeg) => JpegDecoder::new(cursor()).and_then(DynamicImage::from_decoder),
        Ok(ImageFormat::Gif) => GifDecoder::new(cursor()).and_then(DynamicImage::from_decoder),
        Ok(ImageFormat::WebP) => WebPDecoder::new(cursor()).and_then(DynamicImage::from_decoder),
        Ok(other) => {
            return Err(undecodable(&format!(
                "{other:?} images are not decoded here"
            )));
        }
        Err(_) => {
            return Err(undecodable(
                "the bytes are not a PNG, JPEG, GIF or WebP image",
            ));
        }
    };
    Ok(decoded.map_err(|e| Error::runtime(&e))?.into_rgba8())
}

/// Without the `decode` feature every call fails, so hosts decode with their own codecs.
///
/// # Errors
/// Always returns `INVALID_ARGUMENT`.
#[cfg(not(feature = "decode"))]
pub fn decode(_bytes: &[u8]) -> Result<RgbaImage, Error> {
    Err(undecodable(
        "this dith-wasm build has no image decoders (cargo feature `decode`)",
    ))
}

/// Draw a procedural source image.
#[must_use]
pub fn generate(opts: &GenerateOptions) -> RgbaImage {
    generate_pattern(opts)
}

/// Encode `img` to file bytes. `format` is one of [`FORMATS`]; `export` carries the recipe's
/// SVG and PNG settings (`svg_shape`, `svg_skip_bg`, `optimize`). The bytes match what
/// `dith apply -o file.<format>` writes for the same pixels.
///
/// # Errors
/// Returns `INVALID_ARGUMENT` for a format or option this build leaves out.
pub fn encode(img: &RgbaImage, format: &str, export: &ExportOptions) -> Result<Vec<u8>, Error> {
    match format {
        "png" if export.optimize => optimized_png(img),
        "png" => encode_png_bytes(img).map_err(|e| Error::runtime(&e)),
        "svg" => Ok(render_svg(img, export.svg_shape, export.svg_skip_bg).into_bytes()),
        #[cfg(feature = "gif")]
        "gif" => dith_core::output::encode_gif_bytes(img).ok_or_else(|| {
            Error::invalid(
                "format",
                "GIF holds at most 256 colors and 65535 pixels a side".to_string(),
            )
        }),
        #[cfg(feature = "webp")]
        "webp" => dith_core::output::encode_webp_bytes(img).map_err(|e| Error::runtime(&e)),
        other => Err(Error::unsupported(
            "format",
            format!("'{other}' is not built into this dith-wasm"),
            FORMATS,
        )),
    }
}

#[cfg(feature = "png-optimize")]
fn optimized_png(img: &RgbaImage) -> Result<Vec<u8>, Error> {
    dith_core::output::encode_optimized_png_bytes(img).map_err(|e| Error::runtime(&e))
}

#[cfg(not(feature = "png-optimize"))]
fn optimized_png(_: &RgbaImage) -> Result<Vec<u8>, Error> {
    Err(Error::invalid(
        "optimize",
        "PNG optimization is not built into this dith-wasm (cargo feature `png-optimize`)"
            .to_string(),
    ))
}

/// Rasterize an SVG document at `width` x `height`, or at its natural size when either is 0.
///
/// # Errors
/// Returns `RUNTIME_ERROR` when the document is not valid SVG, or `INVALID_ARGUMENT` for a canvas
/// larger than [`MAX_CANVAS`].
#[cfg(feature = "svg")]
pub fn rasterize_svg(svg: &[u8], width: u32, height: u32) -> Result<RgbaImage, Error> {
    let (width, height) = if width == 0 || height == 0 {
        dith_core::svg::svg_size(svg).map_err(|e| Error::runtime(&e))?
    } else {
        (width, height)
    };
    if width > MAX_CANVAS || height > MAX_CANVAS {
        return Err(Error::invalid(
            "width",
            format!("SVG canvas must be at most {MAX_CANVAS} a side (got {width}x{height})"),
        ));
    }
    dith_core::svg::rasterize(svg, width, height).map_err(|e| Error::runtime(&e))
}

/// Built-in presets with their full recipes.
#[must_use]
pub fn presets() -> Value {
    all_presets()
        .iter()
        .map(|p| {
            json!({
                "id": p.id,
                "name": p.name,
                "description": p.description,
                "tags": p.tags,
                "algorithm": p.algorithm.id(),
                "recipe": p.to_recipe(),
            })
        })
        .collect()
}

/// Built-in palettes with their colors.
#[must_use]
pub fn palettes() -> Value {
    json!(list_builtin_palettes())
}

/// Dither algorithms and what they suit.
#[must_use]
pub fn algorithms() -> Value {
    json!(Algorithm::ALL.iter().map(|a| a.info()).collect::<Vec<_>>())
}

/// Procedural generators and how they move.
#[must_use]
pub fn generators() -> Value {
    json!(Pattern::ALL.iter().map(|p| p.info()).collect::<Vec<_>>())
}

/// Metadata about a dither result, everything but the pixels.
#[must_use]
pub fn result_meta(out: &PipelineOutput) -> Value {
    json!({
        "width": out.output_width,
        "height": out.output_height,
        "palette": out.palette.hex_colors(),
        "histogram": out.histogram,
        "source": { "width": out.source_width, "height": out.source_height },
        "working": { "width": out.working_width, "height": out.working_height },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_recipe_json_fills_cli_defaults() {
        let full = recipe(&serde_json::to_string(&DitherRecipe::default()).unwrap()).unwrap();
        assert_eq!(full, DitherRecipe::default());

        let partial = recipe(r#"{"dither":{"algorithm":"atkinson"}}"#).unwrap();
        let mut expected = DitherRecipe::default();
        expected.dither.algorithm = Algorithm::Atkinson;
        assert_eq!(partial, expected);
    }

    #[test]
    fn complete_preset_recipes_round_trip() {
        for p in all_presets() {
            let json = serde_json::to_string(&p.to_recipe()).unwrap();
            assert_eq!(recipe(&json).unwrap(), p.to_recipe(), "{}", p.id);
            assert_eq!(recipe(p.id).unwrap(), p.to_recipe(), "{}", p.id);
        }
    }

    #[test]
    fn errors_carry_cli_codes() {
        assert_eq!(recipe("no-such-preset").unwrap_err().code, "UNKNOWN_PRESET");
        assert_eq!(recipe("{ nope").unwrap_err().code, "INVALID_RECIPE_JSON");
        let bad = recipe(r#"{"dither":{"strength":9}}"#).unwrap();
        let err = dither(RgbaImage::new(2, 2), &bad).unwrap_err();
        assert_eq!(err.code, "INVALID_ARGUMENT");
        assert!(err.suggestion.contains("dith capabilities"));
        assert_eq!(
            image(vec![0; 15], 2, 2).unwrap_err().code,
            "INVALID_ARGUMENT"
        );
        let err = encode(&RgbaImage::new(1, 1), "tga", &ExportOptions::default()).unwrap_err();
        assert_eq!(
            err.valid_values.as_deref().map(<[String]>::len),
            Some(FORMATS.len())
        );
    }

    #[cfg(feature = "decode")]
    #[test]
    fn decode_matches_the_cli_reader() {
        let img =
            generate(&generate_options(r#"{"pattern":"plasma","width":40,"height":30}"#).unwrap());
        let png = encode(&img, "png", &ExportOptions::default()).unwrap();
        assert_eq!(decode(&png).unwrap(), img);
        let jpeg = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../assets/intro-poster.jpg"
        ))
        .unwrap();
        assert_eq!(
            decode(&jpeg).unwrap(),
            dith_core::input::decode_image_bytes(&jpeg, None).unwrap()
        );
        assert_eq!(
            decode(b"BM not really").unwrap_err().code,
            "INVALID_ARGUMENT"
        );
        assert_eq!(
            decode(b"nope").unwrap_err().valid_values.unwrap().len(),
            DECODERS.len()
        );
    }

    #[test]
    fn png_bytes_match_the_cli_encoder() {
        let img =
            generate(&generate_options(r#"{"pattern":"mesh","width":96,"height":64}"#).unwrap());
        let out = dither(img, &recipe("gameboy").unwrap()).unwrap();
        let ours = encode(&out.image, "png", &ExportOptions::default()).unwrap();
        let cli = dith_core::output::encode_image(
            &out.image,
            dith_core::OutputFormat::Png,
            &ExportOptions::default(),
        )
        .unwrap();
        assert_eq!(ours, cli);
    }
}
