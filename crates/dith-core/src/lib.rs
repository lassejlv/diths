//! Dithering engine behind the `dith` CLI.
//!
//! `dith-core` turns RGBA images into palette-limited, dithered output with no terminal,
//! argument-parsing or network code. It covers the whole pipeline the CLI drives:
//!
//! - [`recipe`]: [`DitherRecipe`], the serializable description of one render, and
//!   [`run_pipeline`], which preprocesses, resolves the palette, dithers and finalizes an image.
//! - [`preprocess`], [`palette`], [`dither`] and [`color`]: the individual stages, usable on
//!   their own (for example [`dither_image`] with an explicit [`Palette`]).
//! - [`ink`]: solid ink, clean paper and outline masks layered over any dither ([`SolidInk`]).
//! - [`preset`]: built-in and saved presets that seed a recipe.
//! - [`generate`] and [`animate`]: procedural sources and animated output.
//! - [`output`]: encoders for PNG, WebP, JPEG, GIF, BMP, TIFF, SVG and text/terminal previews.
//! - [`inspect`] and [`compare`]: image analysis and contact sheets.
//!
//! Images are [`image::RgbaImage`]; the [`image`] crate is re-exported so callers use the
//! same version. Enable the `clap` feature to derive `clap::ValueEnum` on the option enums.
//!
//! # Example
//!
//! Dither an in-memory gradient to black and white:
//!
//! ```
//! use dith_core::image::{Rgba, RgbaImage};
//! use dith_core::{Algorithm, DitherRecipe, PaletteSpec, Rgb, run_pipeline};
//!
//! let gradient = RgbaImage::from_fn(64, 16, |x, _| {
//!     let v = (x * 255 / 63) as u8;
//!     Rgba([v, v, v, 255])
//! });
//!
//! let mut recipe = DitherRecipe::default();
//! recipe.palette = PaletteSpec::Builtin("bw".into());
//! recipe.dither.algorithm = Algorithm::Atkinson;
//!
//! let out = run_pipeline(&gradient, &recipe)?;
//! assert_eq!(out.image.dimensions(), (64, 16));
//! assert_eq!(out.palette.colors(), [Rgb::new(0, 0, 0), Rgb::new(255, 255, 255)]);
//! assert!(out.image.pixels().all(|p| p.0 == [0, 0, 0, 255] || p.0 == [255, 255, 255, 255]));
//! # Ok::<(), dith_core::RecipeError>(())
//! ```

pub mod animate;
pub mod color;
pub mod compare;
pub mod diff;
pub mod dither;
pub mod generate;
pub mod inflate;
pub mod ink;
pub mod input;
pub mod inspect;
pub mod output;
pub mod overlay;
pub mod palette;
pub mod preprocess;
pub mod preset;
pub mod recipe;
pub mod resample;
#[cfg(feature = "svg")]
pub mod svg;

mod clock;
mod deflate;
mod gif_lzw;
mod huffman;
mod nearest;
mod png_optimize;
mod webp;

pub use image;

pub use color::{ColorSpace, Rgb};
pub use dither::{Algorithm, DitherOptions, dither_image};
pub use generate::{GenerateOptions, Pattern, generate_pattern};
pub use ink::SolidInk;
pub use output::{ExportOptions, OutputFormat, encode_image};
pub use palette::{Palette, PaletteError};
pub use preprocess::PreprocessOptions;
pub use recipe::{
    DitherRecipe, PaletteSpec, PipelineOutput, RecipeError, run_pipeline, run_pipeline_owned,
};
