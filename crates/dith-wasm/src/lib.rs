//! WebAssembly bindings for [`dith_core`]: dither RGBA pixels in a browser or Node with the same
//! recipe JSON the `dith` CLI reads and writes.
//!
//! The JavaScript API is small: [`dither`], [`generate`], [`encode`], the catalogs
//! ([`presets`], [`palettes`], [`algorithms`], [`generators`]) and [`preset`]. Pixels cross the
//! boundary once each way, as a `Uint8Array`/`Uint8ClampedArray` in and a `Uint8ClampedArray`
//! out, so a result goes straight into `new ImageData(result.rgba, result.width, result.height)`.
//!
//! Errors are thrown as `DithError` objects carrying the CLI's JSON envelope fields: `code`,
//! `message`, `suggestion` and, when known, `validValues`.

pub mod engine;

use dith_core::image::RgbaImage;
use dith_core::output::ExportOptions;
use dith_core::recipe::DitherRecipe;
use js_sys::{JSON, Reflect, Uint8Array, Uint8ClampedArray};
use serde::Serialize;
use wasm_bindgen::prelude::*;

#[wasm_bindgen(typescript_custom_section)]
const TYPES: &str = r##"
/** A dith recipe, as `dith apply --emit-recipe` writes it. Every field is optional; missing
 * fields take the CLI's defaults. */
export interface Recipe {
  /** A label naming the preset the recipe came from. It does not load the preset; use `preset()`. */
  preset?: string | null;
  palette?: PaletteSpec;
  dither?: Partial<DitherOptions>;
  preprocess?: Partial<PreprocessOptions>;
  export?: Partial<ExportOptions>;
}

/** An sRGB color, as recipes store it. */
export interface Rgb {
  r: number;
  g: number;
  b: number;
}

/** A built-in palette id (see `palettes()`), explicit hex colors, or N colors extracted from the image. */
export type PaletteSpec =
  | { mode: "builtin"; value: string }
  | { mode: "colors"; value: Rgb[] }
  | { mode: "adaptive"; value: number };

export interface DitherOptions {
  /** An id from `algorithms()`, for example "floyd-steinberg", "atkinson", "bayer8", "blue-noise". */
  algorithm: string;
  color_space: "oklab" | "linear" | "srgb";
  strength: number;
  serpentine: boolean;
  bias: number;
  noise: number;
  pattern_scale: number;
  pattern_offset: [number, number];
  seed: number;
  ink_below?: number;
  paper_above?: number;
  ink_lines?: number;
}

export interface PreprocessOptions {
  width: number | null;
  height: number | null;
  max_dim: number | null;
  scale: number | null;
  fit: "contain" | "cover" | "stretch";
  pixel_size: number;
  brightness: number;
  contrast: number;
  gamma: number;
  saturation: number;
  sharpen: number;
  blur: number;
  auto_levels: boolean;
  grayscale: boolean;
  invert: boolean;
  alpha_mode: "preserve" | "flatten" | "threshold" | "dither";
  background: Rgb;
}

export interface ExportOptions {
  format: string | null;
  svg_shape: "rect" | "circle" | "dot";
  svg_skip_bg: boolean;
  optimize: boolean;
}

/** Options for `generate()`. Every field is optional. */
export interface GenerateOptions {
  /** An id from `generators()`, for example "sphere", "mesh", "plasma". */
  pattern?: string;
  width?: number;
  height?: number;
  angle?: number;
  frequency?: number;
  /** Color stops, interpolated in Oklab. */
  stops?: Rgb[];
  seed?: number;
  phase?: number;
  motion_amplitude?: number;
  inflate?: Partial<{ depth: number; specular: number; rim: number; ambient: number; blend: number; ground_shadow: number }>;
}

/** RGBA pixels, ready for `new ImageData(rgba, width, height)`. */
export interface RgbaImage {
  rgba: Uint8ClampedArray<ArrayBuffer>;
  width: number;
  height: number;
}

export interface DitherResult extends RgbaImage {
  /** The resolved palette as hex colors. */
  palette: string[];
  histogram: { hex: string; pixels: number; percent: number }[];
  source: { width: number; height: number };
  working: { width: number; height: number };
}

export interface PresetInfo {
  id: string;
  name: string;
  description: string;
  tags: string[];
  algorithm: string;
  recipe: Recipe;
}

export interface PaletteInfo {
  id: string;
  name: string;
  category: string;
  description: string;
  tags: string[];
  colors: string[];
}

export interface AlgorithmInfo {
  id: string;
  name: string;
  category: string;
  description: string;
  best_for: string;
  supports_serpentine: boolean;
  supports_seed: boolean;
  temporally_stable: boolean;
}

export interface GeneratorInfo {
  id: string;
  name: string;
  description: string;
  best_for: string;
  motion: string;
}

/** Thrown by every call: the dith CLI's JSON error envelope as an Error. */
export interface DithError extends Error {
  name: "DithError";
  /** Machine code, for example "UNKNOWN_PRESET" or "INVALID_ARGUMENT". */
  code: string;
  suggestion: string;
  validValues?: string[];
}
"##;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(typescript_type = "Recipe | string")]
    pub type RecipeArg;
    #[wasm_bindgen(typescript_type = "GenerateOptions | string")]
    pub type GenerateOptionsArg;
    #[wasm_bindgen(typescript_type = "Partial<ExportOptions>")]
    pub type ExportOptionsArg;
}

fn throw(err: engine::Error) -> JsValue {
    let engine::Error {
        code,
        message,
        suggestion,
        valid_values,
    } = err;
    let js = js_sys::Error::new(&message);
    js.set_name("DithError");
    let _ = Reflect::set(&js, &"code".into(), &code.into());
    let _ = Reflect::set(&js, &"suggestion".into(), &suggestion.into());
    if let Some(values) = valid_values {
        let _ = Reflect::set(&js, &"validValues".into(), &to_js(&values));
    }
    js.into()
}

fn to_js(value: &impl Serialize) -> JsValue {
    serde_json::to_string(value)
        .ok()
        .and_then(|text| JSON::parse(&text).ok())
        .unwrap_or(JsValue::NULL)
}

/// The JSON text of a string or plain-object argument; `None` for `undefined` or `null`.
fn json_arg(value: &JsValue) -> Result<Option<String>, JsValue> {
    if value.is_undefined() || value.is_null() {
        return Ok(None);
    }
    if let Some(text) = value.as_string() {
        return Ok(Some(text));
    }
    JSON::stringify(value)
        .map(|s| Some(String::from(s)))
        .map_err(|_| {
            throw(
                dith_core::RecipeError::InvalidRecipeJson(
                    "argument is not JSON-serializable".to_string(),
                )
                .into(),
            )
        })
}

fn recipe_arg(value: &JsValue) -> Result<DitherRecipe, JsValue> {
    json_arg(value)?.map_or_else(
        || Ok(DitherRecipe::default()),
        |spec| engine::recipe(&spec).map_err(throw),
    )
}

fn image_result(img: &RgbaImage, meta: JsValue) -> JsValue {
    let rgba = Uint8ClampedArray::from(img.as_raw().as_slice());
    let _ = Reflect::set(&meta, &"rgba".into(), &rgba);
    meta
}

/// Dither RGBA pixels with a recipe: a built-in preset name, recipe JSON text, or a recipe object.
///
/// # Errors
/// Throws a `DithError` for a bad buffer, recipe or palette.
#[wasm_bindgen(unchecked_return_type = "DitherResult")]
pub fn dither(
    #[wasm_bindgen(unchecked_param_type = "Uint8Array | Uint8ClampedArray")] rgba: Vec<u8>,
    width: u32,
    height: u32,
    recipe: &RecipeArg,
) -> Result<JsValue, JsValue> {
    let recipe = recipe_arg(recipe)?;
    let img = engine::image(rgba, width, height).map_err(throw)?;
    let out = engine::dither(img, &recipe).map_err(throw)?;
    Ok(image_result(&out.image, to_js(&engine::result_meta(&out))))
}

/// Draw a procedural source (`generators()` lists them). With a recipe, the source is dithered
/// and a `DitherResult` returned; without one, the raw source pixels.
///
/// # Errors
/// Throws a `DithError` for bad options or a bad recipe.
#[wasm_bindgen(unchecked_return_type = "DitherResult | RgbaImage")]
pub fn generate(
    options: &GenerateOptionsArg,
    recipe: Option<RecipeArg>,
) -> Result<JsValue, JsValue> {
    let opts = engine::generate_options(&json_arg(options)?.unwrap_or_else(|| "{}".to_string()))
        .map_err(throw)?;
    let source = engine::generate(&opts);
    let Some(spec) = json_arg(&recipe.map_or(JsValue::UNDEFINED, Into::into))? else {
        let meta =
            to_js(&serde_json::json!({ "width": source.width(), "height": source.height() }));
        return Ok(image_result(&source, meta));
    };
    let recipe = engine::recipe(&spec).map_err(throw)?;
    let out = engine::dither(source, &recipe).map_err(throw)?;
    Ok(image_result(&out.image, to_js(&engine::result_meta(&out))))
}

/// Encode RGBA pixels as file bytes: `"png"` and `"svg"` always, plus `"gif"` and `"webp"` when
/// built with those features (`formats()` lists them). `options` is a recipe's `export`
/// object (`svg_shape`, `svg_skip_bg`, `optimize`).
///
/// # Errors
/// Throws a `DithError` for a bad buffer or a format this build leaves out.
#[wasm_bindgen]
pub fn encode(
    #[wasm_bindgen(unchecked_param_type = "Uint8Array | Uint8ClampedArray")] rgba: Vec<u8>,
    width: u32,
    height: u32,
    format: &str,
    options: Option<ExportOptionsArg>,
) -> Result<Uint8Array, JsValue> {
    let export = match json_arg(&options.map_or(JsValue::UNDEFINED, Into::into))? {
        Some(spec) => {
            engine::recipe(&format!(r#"{{"export":{spec}}}"#))
                .map_err(throw)?
                .export
        }
        None => ExportOptions::default(),
    };
    let img = engine::image(rgba, width, height).map_err(throw)?;
    let bytes = engine::encode(&img, format, &export).map_err(throw)?;
    Ok(Uint8Array::from(bytes.as_slice()))
}

/// Rasterize an SVG document to RGBA at `width` x `height` (its natural size when omitted).
///
/// # Errors
/// Throws a `DithError` when the document is not valid SVG.
#[cfg(feature = "svg")]
#[wasm_bindgen(js_name = "rasterizeSvg", unchecked_return_type = "RgbaImage")]
pub fn rasterize_svg(
    #[wasm_bindgen(unchecked_param_type = "string | Uint8Array")] svg: &JsValue,
    width: Option<u32>,
    height: Option<u32>,
) -> Result<JsValue, JsValue> {
    let bytes = svg
        .as_string()
        .map_or_else(|| Uint8Array::new(svg).to_vec(), String::into_bytes);
    let img =
        engine::rasterize_svg(&bytes, width.unwrap_or(0), height.unwrap_or(0)).map_err(throw)?;
    let meta = to_js(&serde_json::json!({ "width": img.width(), "height": img.height() }));
    Ok(image_result(&img, meta))
}

/// Decode PNG, JPEG, GIF (first frame) or WebP bytes to RGBA pixels, with the same decoders the
/// dith CLI reads files with.
///
/// # Errors
/// Throws a `DithError` for another format or corrupt data.
#[wasm_bindgen(unchecked_return_type = "RgbaImage")]
pub fn decode(bytes: &[u8]) -> Result<JsValue, JsValue> {
    let img = engine::decode(bytes).map_err(throw)?;
    let meta = to_js(&serde_json::json!({ "width": img.width(), "height": img.height() }));
    Ok(image_result(&img, meta))
}

/// Encoded formats `decode` reads.
#[wasm_bindgen(unchecked_return_type = "string[]")]
#[must_use]
pub fn decoders() -> JsValue {
    to_js(&engine::DECODERS)
}

/// Resolve a recipe argument (a preset name, recipe JSON text or a recipe object) to the complete
/// recipe `dith apply --emit-recipe` would print, every field filled in.
///
/// # Errors
/// Throws a `DithError` for an unknown preset or bad JSON.
#[wasm_bindgen(unchecked_return_type = "Recipe")]
pub fn recipe(spec: &RecipeArg) -> Result<JsValue, JsValue> {
    Ok(to_js(&recipe_arg(spec)?))
}

/// The recipe of a built-in preset, to adjust and pass to `dither`. With `procedural`, photo
/// preparation is dropped, as `dith generate --preset` does.
///
/// # Errors
/// Throws a `DithError` with code `UNKNOWN_PRESET`.
#[wasm_bindgen(unchecked_return_type = "Recipe")]
pub fn preset(name: &str, procedural: Option<bool>) -> Result<JsValue, JsValue> {
    let recipe = engine::preset(name, procedural.unwrap_or(false)).map_err(throw)?;
    Ok(to_js(&recipe))
}

/// Built-in presets, each with its full recipe.
#[wasm_bindgen(unchecked_return_type = "PresetInfo[]")]
#[must_use]
pub fn presets() -> JsValue {
    to_js(&engine::presets())
}

/// Built-in palettes with their hex colors.
#[wasm_bindgen(unchecked_return_type = "PaletteInfo[]")]
#[must_use]
pub fn palettes() -> JsValue {
    to_js(&engine::palettes())
}

/// Dither algorithms.
#[wasm_bindgen(unchecked_return_type = "AlgorithmInfo[]")]
#[must_use]
pub fn algorithms() -> JsValue {
    to_js(&engine::algorithms())
}

/// Procedural generators for `generate`.
#[wasm_bindgen(unchecked_return_type = "GeneratorInfo[]")]
#[must_use]
pub fn generators() -> JsValue {
    to_js(&engine::generators())
}

/// Output formats `encode` accepts in this build.
#[wasm_bindgen(unchecked_return_type = "string[]")]
#[must_use]
pub fn formats() -> JsValue {
    to_js(&engine::FORMATS)
}

/// The dith-core version this module was built from.
#[wasm_bindgen]
#[must_use]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}
