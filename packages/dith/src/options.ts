import { DithError, invalid } from "./errors.ts";
import { CHOICES, DITHER_FIELDS, GENERATE_FIELDS, type DitherOptions, type GenerateOptions } from "./generated.ts";
import type { Recipe, Rgb } from "./types.ts";
import { type Glue, call } from "./wasm.ts";

const HEX = /^#?([\da-f]{3}|[\da-f]{6}|[\da-f]{8})$/i;

/** A hex color as the CLI parses it: `#rgb`, `#rrggbb` or `#rrggbbaa` (alpha ignored), `#` optional. */
function rgb(text: string): Rgb {
  const hex = HEX.exec(text.trim())?.[1];
  if (!hex) {
    throw new DithError("INVALID_HEX_COLOR", `invalid hex color '${text.trim()}'`, "Use hex format like #111111, #f5efe6, or shorthand #000.");
  }
  const full = hex.length === 3 ? [...hex].map((c) => c + c).join("") : hex;
  return { r: parseInt(full.slice(0, 2), 16), g: parseInt(full.slice(2, 4), 16), b: parseInt(full.slice(4, 6), 16) };
}

/** A hex list as `--colors` and `--stops` take it: a string split on commas, semicolons or spaces, or an array. */
function rgbList(spec: string | string[]): Rgb[] {
  const items = Array.isArray(spec) ? spec : spec.split(/[,;\s]+/).filter(Boolean);
  return items.map(rgb);
}

function set(target: Record<string, unknown>, path: string, value: unknown): void {
  const keys = path.split(".");
  const last = keys.pop() as string;
  let node = target;
  for (const key of keys) node = (node[key] ??= {}) as Record<string, unknown>;
  node[last] = value;
}

const flag = (name: string) => name.replace(/[A-Z]/g, (c) => `-${c.toLowerCase()}`);

/** Reject unknown option names, which are typos more often than not. */
function checkNames(options: object, known: readonly string[]): void {
  for (const key of Object.keys(options)) {
    if (!known.includes(key)) {
      throw invalid(key, "unknown option", known, `Options are the dith CLI flags in camelCase (--pixel-size is pixelSize). Valid: ${known.join(", ")}.`);
    }
  }
}

/**
 * The full recipe for `options`, layered like the CLI: `recipe`, then `preset` (which replaces it),
 * then every other option. `procedural` drops a preset's photo preparation, as `dith generate` does.
 */
export function buildRecipe(w: Glue, options: DitherOptions, procedural: boolean): Recipe {
  let recipe = call(() => w.recipe(options.recipe ?? {})) as Recipe;
  if (options.preset !== undefined) recipe = call(() => w.preset(options.preset as string, procedural)) as Recipe;
  const { colors, autoPalette, palette } = options;
  if (colors !== undefined) recipe.palette = { mode: "colors", value: rgbList(colors) };
  else if (autoPalette !== undefined) recipe.palette = { mode: "adaptive", value: autoPalette };
  else if (palette !== undefined) recipe.palette = { mode: "builtin", value: palette };
  for (const [name, value] of Object.entries(options)) {
    const path = DITHER_FIELDS[name];
    if (value === undefined || !path || path === "palette") continue;
    const choices = CHOICES[name];
    if (choices && !choices.includes(value as string)) throw invalid(flag(name), `'${String(value)}' is not one of the choices`, choices);
    set(recipe as Record<string, unknown>, path, name === "background" ? rgb(value as string) : value);
  }
  return recipe;
}

/** Split `generate()` options into the recipe and the generator options `dith generate` would use. */
export function buildGenerate(w: Glue, pattern: string, options: GenerateOptions): { recipe: Recipe; source: Record<string, unknown> } {
  checkNames(options, [...Object.keys(DITHER_FIELDS), ...Object.keys(GENERATE_FIELDS)]);
  const ditherOptions: Record<string, unknown> = {};
  const source: Record<string, unknown> = { pattern, stops: rgbList("#0a0a0d,#f7f7f9") };
  for (const [name, value] of Object.entries(options)) {
    if (value === undefined) continue;
    const field = GENERATE_FIELDS[name];
    if (!field) ditherOptions[name] = value;
    else set(source, field, name === "stops" ? rgbList(value as string | string[]) : value);
  }
  const recipe = buildRecipe(w, ditherOptions as DitherOptions, true);
  const pre = recipe.preprocess ?? {};
  source.width = pre.width ?? source.width ?? 640;
  source.height = pre.height ?? source.height ?? 480;
  source.seed = recipe.dither?.seed;
  pre.width = null;
  pre.height = null;
  return { recipe, source };
}

/** Check `dither()` option names. */
export function checkDitherOptions(options: DitherOptions): void {
  checkNames(options, Object.keys(DITHER_FIELDS));
}
