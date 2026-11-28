/**
 * dith: deterministic dithering in WebAssembly for browsers, Node, Bun, Deno and Workers.
 *
 * ```ts
 * import { dither } from "dith";
 * const out = await dither(imageOrBytesOrUrl, { preset: "manga" });
 * out.toPNG(); // the same bytes `dith apply --preset manga` writes
 * ```
 *
 * @module
 */

import { invalid } from "./errors.ts";
import { GENERATORS, type DitherOptions, type GenerateOptions, type Generator, type Preset } from "./generated.ts";
import { toPixels } from "./input.ts";
import { buildGenerate, buildRecipe, checkDitherOptions } from "./options.ts";
import { DitherResult } from "./result.ts";
import type { AlgorithmInfo, GeneratorInfo, Input, PaletteInfo, PresetInfo, Recipe } from "./types.ts";
import { call, ready } from "./wasm.ts";

export { DithError, type DithErrorCode } from "./errors.ts";
export { DitherResult, type Format, type SvgOptions } from "./result.ts";
export { init } from "./wasm.ts";
export type {
  Algorithm,
  AlphaMode,
  ColorSpace,
  DitherOptions,
  Fit,
  GenerateOptions,
  Generator,
  Palette,
  Preset,
  SvgShape,
} from "./generated.ts";
export type * from "./types.ts";

/**
 * Dither an image. `input` is pixels (`ImageData`, `{ data, width, height }`), encoded bytes
 * (`Uint8Array`, `ArrayBuffer`, `Blob`, `File`, `Response`), a URL or file path, or a browser
 * image source. `options` are the `dith apply` flags in camelCase, or just a preset name.
 *
 * ```ts
 * const out = await dither(file, { preset: "risograph", maxDim: 960 });
 * const one = await dither(file, "mac-classic");
 * const own = await dither(file, { algorithm: "atkinson", colors: ["#0f172a", "#f5efe6"] });
 * ```
 *
 * The same input and options give the same pixels in every runtime and in the CLI.
 * @throws {DithError} for unreadable input, unknown options or out-of-range values.
 */
export async function dither(input: Input, options: DitherOptions | Preset = {}): Promise<DitherResult> {
  const w = await ready();
  const opts: DitherOptions = typeof options === "string" ? { preset: options } : options;
  checkDitherOptions(opts);
  const recipe = buildRecipe(w, opts, false);
  const pixels = await toPixels(w, input);
  return new DitherResult(w, call(() => w.dither(pixels.data, pixels.width, pixels.height, recipe as never)), recipe);
}

/**
 * Draw a procedural image and dither it, as `dith generate <kind>` does: gradients, spheres, meshes,
 * noise and more, with no input image. Presets drop their photo preparation here, as in the CLI.
 *
 * ```ts
 * const orb = await generate("sphere", { preset: "risograph", canvasWidth: 800, canvasHeight: 600 });
 * ```
 * @throws {DithError} for an unknown generator, unknown options or out-of-range values.
 */
export async function generate(kind: Generator, options: GenerateOptions = {}): Promise<DitherResult> {
  const w = await ready();
  if (!GENERATORS.includes(kind)) throw invalid("pattern", `unknown generator '${String(kind)}'`, GENERATORS);
  const { recipe, source } = buildGenerate(w, kind, options);
  return new DitherResult(w, call(() => w.generate(source as never, recipe as never)), recipe);
}

/**
 * The complete recipe `options` resolve to, without rendering anything: what
 * `dith apply --emit-recipe` prints. Save it as JSON to replay the look in the CLI.
 */
export async function recipe(options: DitherOptions | Preset = {}): Promise<Recipe> {
  const w = await ready();
  const opts: DitherOptions = typeof options === "string" ? { preset: options } : options;
  checkDitherOptions(opts);
  return buildRecipe(w, opts, false);
}

/** Built-in presets, each with its description, tags and full recipe. */
export async function presets(): Promise<PresetInfo[]> {
  return (await ready()).presets() as unknown as PresetInfo[];
}

/** Built-in palettes with their hex colors. */
export async function palettes(): Promise<PaletteInfo[]> {
  return (await ready()).palettes() as PaletteInfo[];
}

/** Dither algorithms, with what each suits. */
export async function algorithms(): Promise<AlgorithmInfo[]> {
  return (await ready()).algorithms() as AlgorithmInfo[];
}

/** Procedural generators for `generate()`. */
export async function generators(): Promise<GeneratorInfo[]> {
  return (await ready()).generators() as GeneratorInfo[];
}
