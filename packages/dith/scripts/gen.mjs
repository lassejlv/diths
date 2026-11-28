#!/usr/bin/env node
// Generate src/generated.ts from `dith capabilities`: the preset, palette, algorithm and generator
// unions, every option with its CLI flag, recipe field, range and default, and the runtime table
// that maps options onto a recipe. Run it after changing dith's catalogs or flags.
//
//   node scripts/gen.mjs            write src/generated.ts
//   node scripts/gen.mjs --check    exit 1 when src/generated.ts is stale (CI)
//
// The dith binary is $DITH, else target/release/dith in the repository, else `cargo run`.

import { execFileSync } from "node:child_process";
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";

const here = resolve(import.meta.dirname, "..");
const root = resolve(here, "../..");
const out = resolve(here, "src/generated.ts");

function capabilities() {
  const local = resolve(root, "target/release/dith");
  const [cmd, args] = process.env.DITH
    ? [process.env.DITH, ["capabilities"]]
    : existsSync(local)
      ? [local, ["capabilities"]]
      : ["cargo", ["run", "--quiet", "--release", "--locked", "-p", "dith-cli", "--", "capabilities"]];
  return JSON.parse(execFileSync(cmd, args, { cwd: root, encoding: "utf8", maxBuffer: 1 << 26 }));
}

const caps = capabilities();

// Every option: [name, CLI flag, recipe path, TypeScript type, description when capabilities has
// none]. Names are the flags in camelCase. Recipe paths are where the option lands in the recipe.
const DITHER = [
  ["preset", "--preset", null, "Preset", "Start from a built-in preset (see `presets()`). Like the CLI, it replaces `recipe`, and every other option overrides it."],
  ["recipe", "--recipe", null, "Recipe | string", "Base settings: a recipe object or its JSON text, as `--emit-recipe` and `result.recipe` give it. Missing fields take the CLI's defaults."],
  ["algorithm", "--algorithm", "dither.algorithm", "Algorithm", "Dithering algorithm (see `algorithms()`)."],
  ["palette", "--palette", "palette", "Palette", "Built-in palette id (see `palettes()`), or `grayN` for N gray levels."],
  ["colors", "--colors", "palette", "string | string[]", "Custom palette as hex colors: `\"#111111,#f5efe6\"` or `[\"#111\", \"#eee\"]`. Wins over `palette`."],
  ["autoPalette", "--auto-palette", "palette", "number", null],
  ["colorSpace", "--color-space", "dither.color_space", "ColorSpace", "Color space for error diffusion and nearest-color matching."],
  ["strength", "--strength", "dither.strength", "number", null],
  ["serpentine", "--serpentine", "dither.serpentine", "boolean", "Alternate scan direction each row for error diffusion (`false` is `--no-serpentine`)."],
  ["bias", "--bias", "dither.bias", "number", null],
  ["noise", "--noise", "dither.noise", "number", null],
  ["patternScale", "--pattern-scale", "dither.pattern_scale", "number", null],
  ["patternOffset", null, "dither.pattern_offset", "[number, number]", "Shift of ordered and halftone screens in pixels, `[x, y]` (recipe field only)."],
  ["seed", "--seed", "dither.seed", "number", "Deterministic seed for blue-noise, random, ign, noise and auto-palette."],
  ["inkBelow", "--ink-below", "dither.ink_below", "number", null],
  ["paperAbove", "--paper-above", "dither.paper_above", "number", null],
  ["inkLines", "--ink-lines", "dither.ink_lines", "number", null],
  ["width", "--width", "preprocess.width", "number", "Target output width in pixels."],
  ["height", "--height", "preprocess.height", "number", "Target output height in pixels."],
  ["maxDim", "--max-dim", "preprocess.max_dim", "number", "Cap the longest side at N pixels, keeping the aspect ratio."],
  ["scale", "--scale", "preprocess.scale", "number", "Uniform resize multiplier, for example 0.5 or 2."],
  ["fit", "--fit", "preprocess.fit", "Fit", "How to fit when both `width` and `height` are set."],
  ["pixelSize", "--pixel-size", "preprocess.pixel_size", "number", null],
  ["brightness", "--brightness", "preprocess.brightness", "number", null],
  ["contrast", "--contrast", "preprocess.contrast", "number", null],
  ["gamma", "--gamma", "preprocess.gamma", "number", null],
  ["saturation", "--saturation", "preprocess.saturation", "number", null],
  ["sharpen", "--sharpen", "preprocess.sharpen", "number", null],
  ["blur", "--blur", "preprocess.blur", "number", null],
  ["autoLevels", "--auto-levels", "preprocess.auto_levels", "boolean", "Stretch the luminance histogram (1st to 99th percentile) before dithering."],
  ["grayscale", "--grayscale", "preprocess.grayscale", "boolean", "Convert to perceptual grayscale before dithering."],
  ["invert", "--invert", "preprocess.invert", "boolean", "Invert tones before dithering."],
  ["alphaMode", "--alpha-mode", "preprocess.alpha_mode", "AlphaMode", "How transparency is handled."],
  ["background", "--background", "preprocess.background", "string", "Hex color transparent pixels are flattened onto with `alphaMode: \"flatten\"`."],
  ["svgShape", "--svg-shape", "export.svg_shape", "SvgShape", "Vector primitive for `toSVG()`."],
  ["svgSkipBg", "--svg-skip-bg", "export.svg_skip_bg", "boolean", "Leave the dominant background color out of `toSVG()`."],
  ["optimize", "--optimize", "export.optimize", "boolean", "Smallest lossless PNG from `toPNG()` (palette, filter and Zopfli search); pixels are identical. Needs a dith-wasm build with the `png-optimize` feature."],
];

// Options of `generate()` beyond the dither options. Defaults are the `dith generate` ones.
const GENERATE = [
  ["canvasWidth", "--canvas-width", "width", "number", "Canvas width in pixels. `width` wins when set, as in the CLI.", "640"],
  ["canvasHeight", "--canvas-height", "height", "number", "Canvas height in pixels. `height` wins when set, as in the CLI.", "480"],
  ["angle", "--angle", "angle", "number", "Direction or light angle in degrees.", "135"],
  ["frequency", "--frequency", "frequency", "number", "Pattern frequency or scale factor.", "1"],
  ["stops", "--stops", "stops", "string | string[]", "Source gradient stops as hex colors, interpolated in Oklab.", "\"#0a0a0d,#f7f7f9\""],
  ["phase", null, "phase", "number", "Loop phase in [0, 1): one frame of the seamless loop `dith animate` draws.", "0"],
  ["motionAmplitude", "--motion-amplitude", "motion_amplitude", "number", "Scales how far procedural motion travels with `phase` (0 = still, 2 = dramatic).", "1"],
  ["depth", "--depth", "inflate.depth", "number", null, null],
  ["specular", "--specular", "inflate.specular", "number", "Inflate: strength of the glossy highlight.", null],
  ["rim", "--rim", "inflate.rim", "number", "Inflate: strength of the rim light along the outline.", null],
  ["ambient", "--ambient", "inflate.ambient", "number", "Inflate: floor brightness of surfaces facing away from the light.", null],
  ["blend", "--blend", "inflate.blend", "number", null, null],
  ["groundShadow", "--ground-shadow", "inflate.ground_shadow", "number", "Inflate: darkness of a soft elliptical shadow under the shape (0 = none).", "0"],
];

// Recipe fields with no option here: terminal text output, the output format (pick the method
// instead) and file paths, which a browser has none of.
const UNMAPPED = ["export.format", "export.text_width", "export.view_cols", "export.view_rows", "preset"];

// Recipe fields left out of the JSON while off (the manga preset sets them).
const OPTIONAL = ["dither.ink_below", "dither.paper_above", "dither.ink_lines"];

const ENUMS = {
  Fit: ["contain", "cover", "stretch"],
  AlphaMode: ["preserve", "flatten", "threshold", "dither"],
  SvgShape: ["rect", "circle", "dot"],
};

// Ranges and effects keyed by flag, splitting combined rows like "--specular / --rim / --ambient".
const ranges = new Map();
for (const row of caps.parameter_ranges) {
  const flags = row.flag.split(" / ");
  const split = (v) => (String(v).split(" / ").length === flags.length ? String(v).split(" / ") : flags.map(() => String(v)));
  const [range, def] = [split(row.range), split(row.default)];
  flags.forEach((flag, i) => ranges.set(flag.trim(), { range: range[i], default: def[i], effect: row.effect.replace(/^inflate: (.)/, (_, c) => `Inflate: ${c}`), bool: row.value_type === "bool" }));
}

const leaves = (value, path = []) =>
  value && typeof value === "object" && !Array.isArray(value) && !("r" in value) && path.join(".") !== "palette"
    ? Object.entries(value).flatMap(([k, v]) => leaves(v, [...path, k]))
    : [path.join(".")];
const at = (obj, path) => path.split(".").reduce((o, k) => o?.[k], obj);

const recipeLeaves = leaves(caps.default_recipe);
const mapped = new Set(DITHER.map((o) => o[2]).filter(Boolean));
const missing = recipeLeaves.filter((p) => !mapped.has(p) && !UNMAPPED.includes(p));
if (missing.length) throw new Error(`recipe fields with no option: ${missing.join(", ")}; add them to scripts/gen.mjs`);
for (const [name, flag, path] of DITHER) {
  if (path && path !== "palette" && !OPTIONAL.includes(path) && !recipeLeaves.includes(path)) {
    throw new Error(`option ${name} maps to ${path}, which the recipe no longer has`);
  }
  if (flag && flag !== "--recipe" && flag !== "--preset" && !ranges.has(flag) && !/^--(algorithm|palette|colors|color-space|serpentine|seed|width|height|max-dim|scale|fit|auto-levels|grayscale|invert|alpha-mode|background|svg-shape|svg-skip-bg)$/.test(flag)) {
    throw new Error(`option ${name}: ${flag} is not a known flag`);
  }
}

const json = (v) => JSON.stringify(v);
const fmt = (v) => (v && typeof v === "object" && "r" in v ? `"#${[v.r, v.g, v.b].map((c) => c.toString(16).padStart(2, "0")).join("")}"` : json(v));
const union = (ids) => ids.map(json).join(" | ");

const sentence = (t) => t.charAt(0).toUpperCase() + t.slice(1) + (/[.)]$/.test(t) ? "" : ".");

function doc([name, flag, path, , text], { generator = false, fallbackDefault } = {}) {
  const r = flag && ranges.get(flag);
  const lines = [sentence(text ?? r?.effect ?? name)];
  if (r?.range && !r.bool) lines.push(`Range \`${r.range}\`.`);
  const recipeDefault = name === "palette" ? caps.default_recipe.palette.value : !generator && path && path !== "palette" ? at(caps.default_recipe, path) : undefined;
  const def = fallbackDefault ?? (recipeDefault != null ? fmt(recipeDefault) : r?.default);
  lines.push(def == null || /^(none|off|null)\b/.test(def) ? "Default: unset." : `Default \`${def}\`.`);
  const where = [flag && `CLI \`${flag}\``, path && path !== "palette" && `${generator ? "generator" : "recipe"} \`${path}\``].filter(Boolean);
  if (where.length) lines.push(where.join(", ") + ".");
  return `  /** ${lines.join(" ")} */`;
}

const presets = caps.presets.filter((p) => p.source === "builtin");
const src = `// Generated by scripts/gen.mjs from \`dith capabilities\` (dith ${caps.version}). Do not edit; run \`bun run gen\`.

import type { Recipe } from "./types.ts";

/** Built-in presets. \`presets()\` returns each with its description and recipe. */
export type Preset = ${union(presets.map((p) => p.id))};
/** Dither algorithms. \`algorithms()\` describes each. */
export type Algorithm = ${union(caps.algorithms.map((a) => a.id))};
/** Built-in palettes, plus \`grayN\` for N evenly spaced grays. \`palettes()\` lists their colors. */
export type Palette = ${union(caps.palettes.map((p) => p.id))} | \`gray\${number}\`;
/** Procedural generators for \`generate()\`. \`generators()\` describes each. */
export type Generator = ${union(caps.generators.map((g) => g.id))};
/** ${caps.color_spaces.map((c) => `\`${c.id}\`: ${c.description}.`).join(" ")} */
export type ColorSpace = ${union(caps.color_spaces.map((c) => c.id))};
${Object.entries(ENUMS).map(([k, v]) => `export type ${k} = ${union(v)};`).join("\n")}

/** Options for \`dither()\` and \`generate()\`: the \`dith apply\` flags in camelCase. Every field is optional. */
export interface DitherOptions {
${DITHER.map((o) => `${doc(o)}\n  ${o[0]}?: ${o[3]};`).join("\n")}
}

/** Options for \`generate()\`: the dither options plus the \`dith generate\` canvas and pattern flags. */
export interface GenerateOptions extends DitherOptions {
${GENERATE.map((o) => `${doc(o, { generator: true, fallbackDefault: o[5] ?? undefined })}\n  ${o[0]}?: ${o[3]};`).join("\n")}
}

/** @internal Option name to recipe path (dither options) or generator field (generate options). */
export const DITHER_FIELDS: Record<string, string | null> = ${json(Object.fromEntries(DITHER.map((o) => [o[0], o[2]])))};
/** @internal */
export const GENERATE_FIELDS: Record<string, string> = ${json(Object.fromEntries(GENERATE.map((o) => [o[0], o[2]])))};
/** @internal Allowed values of the enum options, for errors with \`validValues\`. */
export const CHOICES: Record<string, readonly string[]> = ${json({
  algorithm: caps.algorithms.map((a) => a.id),
  colorSpace: caps.color_spaces.map((c) => c.id),
  fit: ENUMS.Fit,
  alphaMode: ENUMS.AlphaMode,
  svgShape: ENUMS.SvgShape,
})};
/** @internal */
export const GENERATORS: readonly string[] = ${json(caps.generators.map((g) => g.id))};
`;

// The option tables in README.md and llms.txt, between <!-- options:start --> and <!-- options:end -->.
function row([name, flag, path, type], { generator = false, fallbackDefault } = {}) {
  const r = flag && ranges.get(flag);
  const recipeDefault = name === "palette" ? caps.default_recipe.palette.value : !generator && path && path !== "palette" ? at(caps.default_recipe, path) : undefined;
  const def = fallbackDefault ?? (recipeDefault != null ? fmt(recipeDefault) : r?.default);
  const cell = (v) => (v ? "`" + String(v).replaceAll("|", "\\|") + "`" : "");
  const field = path === "palette" ? "`palette`" : path ? cell(path) + (generator ? " (generator)" : "") : "";
  const value = def == null || /^(none|off|null)\b/.test(def) ? "" : cell(def);
  return `| \`${name}\` | ${cell(type)} | ${cell(flag ?? "")} | ${field} | ${r && !r.bool ? cell(r.range) : ""} | ${value} |`;
}
const head = "| Option | Type | CLI flag | Recipe field | Range | Default |\n| --- | --- | --- | --- | --- | --- |";
const tables = [head, ...DITHER.map((o) => row(o)), "", "`generate(kind, options)` also takes:", "", head, ...GENERATE.map((o) => row(o, { generator: true, fallbackDefault: o[5] ?? undefined }))].join("\n");
const docs = ["README.md", "llms.txt"].map((file) => {
  const path = resolve(here, file);
  const text = readFileSync(path, "utf8");
  return [path, text.replace(/<!-- options:start -->[\s\S]*<!-- options:end -->/, `<!-- options:start -->\n${tables}\n<!-- options:end -->`)];
});

const outputs = [[out, src], ...docs];
if (process.argv.includes("--check")) {
  const stale = outputs.filter(([path, text]) => !existsSync(path) || readFileSync(path, "utf8") !== text);
  if (stale.length) {
    console.error(`stale: ${stale.map(([p]) => p.slice(here.length + 1)).join(", ")}; run \`bun run gen\` and commit`);
    process.exit(1);
  }
  console.log("src/generated.ts, README.md and llms.txt are up to date");
} else {
  for (const [path, text] of outputs) writeFileSync(path, text);
  console.log(`wrote src/generated.ts and the README.md and llms.txt option tables from dith ${caps.version}`);
}
