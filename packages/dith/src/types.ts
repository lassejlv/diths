import type { AlphaMode, Algorithm, ColorSpace, Fit, Palette, SvgShape } from "./generated.ts";

/** An sRGB color as recipes store it. Options take hex strings instead. */
export interface Rgb {
  r: number;
  g: number;
  b: number;
}

/**
 * A complete dith recipe: the JSON `dith apply --emit-recipe` prints and `--recipe` reads. Every
 * result carries the one it was rendered with as `result.recipe`, so a render can be replayed
 * here or in the CLI. Recipes passed in may leave out any field; missing ones take the defaults.
 */
export interface Recipe {
  /** Label of the preset the recipe came from. It does not load the preset; use `preset`. */
  preset?: string | null;
  palette?:
    | { mode: "builtin"; value: Palette }
    | { mode: "colors"; value: Rgb[] }
    | { mode: "adaptive"; value: number };
  dither?: {
    algorithm?: Algorithm;
    color_space?: ColorSpace;
    strength?: number;
    serpentine?: boolean;
    bias?: number;
    noise?: number;
    pattern_scale?: number;
    pattern_offset?: [number, number];
    seed?: number;
    ink_below?: number;
    paper_above?: number;
    ink_lines?: number;
  };
  preprocess?: {
    width?: number | null;
    height?: number | null;
    max_dim?: number | null;
    scale?: number | null;
    fit?: Fit;
    pixel_size?: number;
    brightness?: number;
    contrast?: number;
    gamma?: number;
    saturation?: number;
    sharpen?: number;
    blur?: number;
    auto_levels?: boolean;
    grayscale?: boolean;
    invert?: boolean;
    alpha_mode?: AlphaMode;
    background?: Rgb;
  };
  export?: {
    format?: string | null;
    svg_shape?: SvgShape;
    svg_skip_bg?: boolean;
    optimize?: boolean;
    text_width?: number | null;
    view_cols?: number | null;
    view_rows?: number | null;
  };
}

/** Raw RGBA pixels, 4 bytes per pixel, row by row. `ImageData` and every result fit this shape. */
export interface RgbaPixels {
  data: Uint8ClampedArray | Uint8Array;
  width: number;
  height: number;
}

type Instance<K extends string> = typeof globalThis extends Record<K, abstract new (...args: never[]) => infer T> ? T : never;

/** Browser image sources, decoded by drawing them onto a canvas. Absent outside browsers. */
export type DrawableInput =
  | Instance<"HTMLImageElement">
  | Instance<"HTMLCanvasElement">
  | Instance<"HTMLVideoElement">
  | Instance<"ImageBitmap">
  | Instance<"OffscreenCanvas">
  | Instance<"VideoFrame">;

/**
 * Anything `dither()` reads:
 * - pixels: `ImageData`, `{ data, width, height }` or a previous result
 * - encoded image bytes (PNG, JPEG, GIF, WebP): `Uint8Array`, `ArrayBuffer`, `Blob`, `File`, `Response`
 * - a URL string or `URL` (fetched), or on Node, Bun and Deno a file path or `file:` URL (read)
 * - in browsers: `HTMLImageElement`, `HTMLCanvasElement`, `HTMLVideoElement`, `ImageBitmap`, `OffscreenCanvas`, `VideoFrame`
 */
export type Input = RgbaPixels | ArrayBuffer | ArrayBufferView | Blob | Response | string | URL | DrawableInput;

/** What `toImageData()` returns: `ImageData` with DOM types (browsers; Deno has it too), else plain pixels. */
export type ImageDataLike = [Instance<"ImageData">] extends [never] ? RgbaPixels : Instance<"ImageData">;

/** A palette color's share of a result. */
export interface ColorUsage {
  hex: string;
  pixels: number;
  percent: number;
}

/** A built-in preset, as `dith list presets --json` shows it. */
export interface PresetInfo {
  id: string;
  name: string;
  description: string;
  tags: string[];
  algorithm: string;
  recipe: Recipe;
}

/** A built-in palette with its colors. */
export interface PaletteInfo {
  id: string;
  name: string;
  category: string;
  description: string;
  tags: string[];
  colors: string[];
}

/** A dither algorithm and what it suits. */
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

/** A procedural generator for `generate()`. */
export interface GeneratorInfo {
  id: string;
  name: string;
  description: string;
  best_for: string;
  motion: string;
}

/** Where the WebAssembly module comes from: a URL, its bytes, a fetch `Response` or a compiled module. */
export type WasmSource = string | URL | Request | Response | Promise<Response> | BufferSource | WebAssembly.Module;

export interface InitOptions {
  /**
   * The .wasm to load. By default the package loads its own: `dith.wasm` in browsers, which decode
   * images with the platform, and `dith-decode.wasm` (PNG, JPEG, GIF and WebP decoders built in)
   * everywhere else. Pass a URL or bytes to host it yourself.
   */
  wasm?: WasmSource;
  /**
   * Load `dith-decode.wasm` in browsers too (115 KB more brotli), so encoded images decode exactly
   * as the dith CLI decodes them instead of with the browser's codecs. Ignored when `wasm` is set.
   */
  decoders?: boolean;
}

/** What the loaded module can do. */
export interface DithInfo {
  /** dith-core version of the module. */
  version: string;
  /** Formats `result.encode()` writes, for example `["png", "svg", "gif"]`. */
  formats: string[];
  /** Encoded formats decoded in WebAssembly; empty for `dith.wasm`, which uses the platform. */
  decoders: string[];
}
