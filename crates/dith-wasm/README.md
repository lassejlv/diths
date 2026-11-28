# dith-wasm

The [dith](https://github.com/lassejlv/diths) dithering engine compiled to WebAssembly, for browsers and Node. It takes the same recipe JSON as the `dith` CLI, so a look you tune on the command line renders the same pixels on a web page.

## Build

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.130   # must match wasm-bindgen in Cargo.lock
# and wasm-opt from binaryen on PATH

crates/dith-wasm/scripts/build.sh                    # light default build into crates/dith-wasm/pkg
crates/dith-wasm/scripts/build.sh --features full    # adds GIF, WebP, PNG optimize and SVG input
```

`pkg/` is an npm package with three entry points: `dith-wasm` (ES module for browsers, call `init()` first), `dith-wasm/bundler` (for bundlers that load `.wasm` imports) and `dith-wasm/node` (CommonJS, ready on import).

## Dither a canvas

```js
import init, { dither, encode, preset } from "dith-wasm";

await init();

const ctx = canvas.getContext("2d");
const src = ctx.getImageData(0, 0, canvas.width, canvas.height);

// A preset name, recipe JSON text, or a recipe object.
const out = dither(src.data, src.width, src.height, "risograph");
canvas.width = out.width;
canvas.height = out.height;
ctx.putImageData(new ImageData(out.rgba, out.width, out.height), 0, 0);

// Start from a preset and change one thing.
const recipe = preset("manga");
recipe.dither.algorithm = "atkinson";
const tweaked = dither(src.data, src.width, src.height, recipe);

// The same PNG bytes `dith apply -o out.png` writes.
const png = encode(tweaked.rgba, tweaked.width, tweaked.height, "png");
const url = URL.createObjectURL(new Blob([png], { type: "image/png" }));
```

In Node, `require("dith-wasm/node")` and pass any RGBA buffer.

## API

| Call | Returns |
| --- | --- |
| `dither(rgba, width, height, recipe)` | `{ rgba, width, height, palette, histogram, source, working }` |
| `generate(options, recipe?)` | A procedural source; dithered when a recipe is given |
| `encode(rgba, width, height, format, options?)` | File bytes: `png`, `svg`, and `gif`/`webp` when built in |
| `preset(name, procedural?)` | A built-in preset's recipe, to adjust |
| `presets()`, `palettes()`, `algorithms()`, `generators()` | The catalogs, as `dith list --json` shows them |
| `formats()`, `version()` | What this build encodes, and the dith-core version |
| `rasterizeSvg(svg, width?, height?)` | RGBA pixels of an SVG (`svg` feature) |

`rgba` is a `Uint8Array` or `Uint8ClampedArray` (an `ImageData`'s `data`), and results return a `Uint8ClampedArray`. Pixels are copied once into wasm memory and once out.

**Recipes** are the JSON `dith apply --emit-recipe` writes and `--recipe` reads. Any field may be left out, nested ones included, and takes the CLI's default. A complete recipe renders exactly as the CLI renders it. As in the CLI, a recipe's `preset` field is only a label: start from `preset(name)` to build on a preset. Palette files and overlay layers are file paths, which a browser has none of, so they fail with `PALETTE_IO_ERROR` or `INVALID_ASSET`.

**Errors** are thrown as `DithError` with the CLI's JSON envelope fields: `code` (`UNKNOWN_PRESET`, `INVALID_ARGUMENT`, ...), `message`, `suggestion` and, when known, `validValues`.

## Cargo features

The default build has the whole engine (every algorithm, palette, preset and generator) with PNG, SVG and GIF output. Heavier parts are opt-in. Sizes are `dith_wasm_bg.wasm` after wasm-opt; the JS glue adds about 8 KB.

| Build | Adds | Raw | gzip -9 | brotli -q 11 |
| --- | --- | ---: | ---: | ---: |
| default (`gif`) | `encode(..., "gif")` (3 KB brotli on its own) | 824 KB | 334 KB | 266 KB |
| `+ webp` | `encode(..., "webp")` | +98 KB | +30 KB | +25 KB |
| `+ png-optimize` | `encode(..., "png", { optimize: true })`: palette and filter search plus Zopfli | +175 KB | +64 KB | +49 KB |
| `+ svg` | `rasterizeSvg()`, through resvg | +875 KB | +355 KB | +239 KB |
| `full` | All of the above | 1,972 KB | 793 KB | 577 KB |

## Same pixels as native

`scripts/parity.mjs` renders 104 cases through the `.wasm` and through native dith-core: every preset, every algorithm on 2- and 16-color palettes, the preprocessing paths and every generator. It requires identical RGBA and identical encoded bytes. CI runs it on Linux.

Eleven cases are known to differ by a few pixels, all of them in paths that call `powf`, `sinf`, `cosf` or `expf`: `gamma`, grayscale, adaptive palettes, `ostromoukhov`, and the `mesh`, `plasma`, `waves`, `radial-gradient` and `inflate` generators. Native builds call the platform's math library (glibc on Linux), while WebAssembly calls Rust's `libm`, and the two can disagree in the last bit, which can flip a dithered pixel. They are pinned by name with a pixel budget in `LIBM_DIFFS` in `scripts/cases.mjs`. Every built-in preset matches exactly.

## Speed

The module runs on one thread and is 1.1–1.6x the time native dith-core takes on one thread (Node and Chromium agree within a few percent). Native error diffusion is sequential too, so Floyd–Steinberg and Atkinson are close to native even against all cores; ordered dithers and preprocessing are where native's extra cores pay off. `scripts/bench.mjs` (Node) and `scripts/bench.html` (browsers) measure it.
