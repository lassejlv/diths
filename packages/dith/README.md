# dith (JavaScript)

The [dith](https://github.com/lassejlv/diths) dithering engine for browsers, Node, Bun, Deno and Cloudflare Workers: WebAssembly behind one typed call, with zero dependencies. The same input and options give the same pixels in every runtime and in the `dith` CLI, byte for byte.

**Browser**

```html
<script type="module">
  import { dither } from "./node_modules/dith/dist/index.js"; // or your bundler, or a CDN
  const out = await dither(document.querySelector("img"), { preset: "manga" });
  document.body.append(Object.assign(new Image(), { src: out.toDataURL() }));
</script>
```

**Node, Bun, Deno**

```js
import { dither } from "dith";
import { writeFile } from "node:fs/promises";
const out = await dither("photo.jpg", { preset: "risograph", maxDim: 960 });
await writeFile("out.png", out.toPNG()); // the bytes `dith apply photo.jpg --preset risograph --max-dim 960` writes
```

The package is in this repository at `packages/dith` and is not published yet. [llms.txt](llms.txt) is this whole API on one page for agents.

## API

| Call | Returns |
| --- | --- |
| `dither(input, options?)` | `Promise<DitherResult>`. `options` is a preset name or the `dith apply` flags in camelCase. |
| `generate(kind, options?)` | `Promise<DitherResult>`: a procedural image (`sphere`, `mesh`, `noise`, ...) dithered, as `dith generate` does. |
| `recipe(options?)` | `Promise<Recipe>`: the complete recipe the options resolve to, as `--emit-recipe` prints it. |
| `presets()`, `palettes()`, `algorithms()`, `generators()` | The catalogs, as `dith list <kind> --json` shows them. |
| `init(options?)` | Loads the WebAssembly early or from your own URL; see [Loading](#loading). Resolves to `{ version, formats, decoders }`. |

**Inputs.** `input` is any of: `ImageData` or `{ data, width, height }` RGBA pixels (a previous result works too); encoded PNG, JPEG, GIF or WebP as `Uint8Array`, `ArrayBuffer`, `Blob`, `File` or `Response`; a URL string or `URL`, which is fetched, or on Node, Bun and Deno a file path or `file:` URL, which is read; and in browsers `HTMLImageElement`, `HTMLCanvasElement`, `HTMLVideoElement`, `ImageBitmap`, `OffscreenCanvas` or `VideoFrame`.

**`DitherResult`** is `{ data, width, height }` pixels plus:

| Member | |
| --- | --- |
| `palette`, `histogram` | Hex colors, and how many pixels use each. |
| `source`, `working` | Input size, and the size the dither ran at (smaller with `pixelSize`). |
| `recipe` | The complete recipe used. `dither(input, { recipe })` or `dith apply --recipe` with it gives the same pixels. |
| `toPNG()`, `toGIF()` | `Uint8Array` file bytes, identical to the CLI's. |
| `toSVG(options?)` | An SVG string; `{ svgShape: "circle" }` for halftone dots. |
| `toImageData()` | `ImageData` where the runtime has it (browsers, Deno), else `{ data, width, height }`. |
| `toBlob(format?)`, `toDataURL(format?)` | For downloads, uploads and `<img src>`. `format` is `"png"` (default), `"gif"` or `"svg"`. |
| `encode(format)` | Bytes of any format the module encodes (`init()` lists them). |

**Errors.** Every failure is a `DithError` with the fields of the CLI's `--json` error envelope: `code` (`INVALID_ARGUMENT`, `UNKNOWN_PRESET`, `UNKNOWN_PALETTE`, `INVALID_HEX_COLOR`, `INVALID_RECIPE_JSON`, `RUNTIME_ERROR`, ...), `message`, `suggestion` and, when the set is known, `validValues`. A misspelt option fails too, listing the valid names. `err.toJSON()` is the envelope itself.

```js
try {
  await dither(file, { preset: "mangaa" });
} catch (err) {
  err.code;        // "UNKNOWN_PRESET"
  err.validValues; // ["blueprint", "crt-amber", ..., "manga", ...]
}
```

## Options

Options are the `dith apply` and `dith generate` flags in camelCase: `--pixel-size 3` is `pixelSize: 3`, `--no-serpentine` is `serpentine: false`, and `--colors "#111,#eee"` is `colors: "#111,#eee"` or `colors: ["#111", "#eee"]`. They layer as in the CLI: `recipe` first, then `preset`, which replaces it, then every other option on top. Every option is typed, with its range and default in the editor; presets, palettes, algorithms and generators are string unions. This table and the types are generated from `dith capabilities`, so they follow the CLI.

<!-- options:start -->
| Option | Type | CLI flag | Recipe field | Range | Default |
| --- | --- | --- | --- | --- | --- |
| `preset` | `Preset` | `--preset` |  |  |  |
| `recipe` | `Recipe \| string` | `--recipe` |  |  |  |
| `algorithm` | `Algorithm` | `--algorithm` | `dither.algorithm` |  | `"floyd-steinberg"` |
| `palette` | `Palette` | `--palette` | `palette` |  | `"bw"` |
| `colors` | `string \| string[]` | `--colors` | `palette` |  |  |
| `autoPalette` | `number` | `--auto-palette` | `palette` | `2..=64` |  |
| `colorSpace` | `ColorSpace` | `--color-space` | `dither.color_space` |  | `"oklab"` |
| `strength` | `number` | `--strength` | `dither.strength` | `0.0..=1.5` | `1` |
| `serpentine` | `boolean` | `--serpentine` | `dither.serpentine` |  | `true` |
| `bias` | `number` | `--bias` | `dither.bias` | `-0.5..=0.5` | `0` |
| `noise` | `number` | `--noise` | `dither.noise` | `0.0..=0.5` | `0` |
| `patternScale` | `number` | `--pattern-scale` | `dither.pattern_scale` | `1..=32` | `1` |
| `patternOffset` | `[number, number]` |  | `dither.pattern_offset` |  | `[0,0]` |
| `seed` | `number` | `--seed` | `dither.seed` |  | `42` |
| `inkBelow` | `number` | `--ink-below` | `dither.ink_below` | `0.0..=1.0` |  |
| `paperAbove` | `number` | `--paper-above` | `dither.paper_above` | `0.0..=1.0` |  |
| `inkLines` | `number` | `--ink-lines` | `dither.ink_lines` | `0.0..=1.0` |  |
| `width` | `number` | `--width` | `preprocess.width` |  |  |
| `height` | `number` | `--height` | `preprocess.height` |  |  |
| `maxDim` | `number` | `--max-dim` | `preprocess.max_dim` |  |  |
| `scale` | `number` | `--scale` | `preprocess.scale` |  |  |
| `fit` | `Fit` | `--fit` | `preprocess.fit` |  | `"contain"` |
| `pixelSize` | `number` | `--pixel-size` | `preprocess.pixel_size` | `1..=64` | `1` |
| `brightness` | `number` | `--brightness` | `preprocess.brightness` | `-1.0..=1.0` | `0` |
| `contrast` | `number` | `--contrast` | `preprocess.contrast` | `-1.0..=1.0` | `0` |
| `gamma` | `number` | `--gamma` | `preprocess.gamma` | `0.1..=5.0` | `1` |
| `saturation` | `number` | `--saturation` | `preprocess.saturation` | `0.0..=3.0` | `1` |
| `sharpen` | `number` | `--sharpen` | `preprocess.sharpen` | `0.0..=5.0` | `0` |
| `blur` | `number` | `--blur` | `preprocess.blur` | `0.0..=20.0` | `0` |
| `autoLevels` | `boolean` | `--auto-levels` | `preprocess.auto_levels` |  | `false` |
| `grayscale` | `boolean` | `--grayscale` | `preprocess.grayscale` |  | `false` |
| `invert` | `boolean` | `--invert` | `preprocess.invert` |  | `false` |
| `alphaMode` | `AlphaMode` | `--alpha-mode` | `preprocess.alpha_mode` |  | `"preserve"` |
| `background` | `string` | `--background` | `preprocess.background` |  | `"#ffffff"` |
| `svgShape` | `SvgShape` | `--svg-shape` | `export.svg_shape` |  | `"rect"` |
| `svgSkipBg` | `boolean` | `--svg-skip-bg` | `export.svg_skip_bg` |  | `false` |
| `optimize` | `boolean` | `--optimize` | `export.optimize` |  | `false` |

`generate(kind, options)` also takes:

| Option | Type | CLI flag | Recipe field | Range | Default |
| --- | --- | --- | --- | --- | --- |
| `canvasWidth` | `number` | `--canvas-width` | `width` (generator) |  | `640` |
| `canvasHeight` | `number` | `--canvas-height` | `height` (generator) |  | `480` |
| `angle` | `number` | `--angle` | `angle` (generator) |  | `135` |
| `frequency` | `number` | `--frequency` | `frequency` (generator) |  | `1` |
| `stops` | `string \| string[]` | `--stops` | `stops` (generator) |  | `"#0a0a0d,#f7f7f9"` |
| `phase` | `number` |  | `phase` (generator) |  | `0` |
| `motionAmplitude` | `number` | `--motion-amplitude` | `motion_amplitude` (generator) |  | `1` |
| `depth` | `number` | `--depth` | `inflate.depth` (generator) | `0.05..=4.0` | `1.0` |
| `specular` | `number` | `--specular` | `inflate.specular` (generator) | `0.0..=2.0` | `0.42` |
| `rim` | `number` | `--rim` | `inflate.rim` (generator) | `0.0..=2.0` | `0.28` |
| `ambient` | `number` | `--ambient` | `inflate.ambient` (generator) | `0.0..=1.0` | `0.08` |
| `blend` | `number` | `--blend` | `inflate.blend` (generator) | `0..=512` | `0` |
| `groundShadow` | `number` | `--ground-shadow` | `inflate.ground_shadow` (generator) | `0.0..=1.0` | `0` |
<!-- options:end -->

Recipe fields with no option: `export.format` (pick the method instead), the terminal-only `text_width`, `view_cols` and `view_rows`, and the file-path features (`--palette-file`, `--overlay`, `--shape`, `--paint`), which have no files to read in a browser.

## Looks

Start from a preset and override single options, as in the [dith skill](../../skills/dith/SKILL.md#choose-a-look).

| Goal | Options |
| --- | --- |
| Classic 1-bit Mac or editorial portrait | `{ preset: "mac-classic" }`, plus `sharpen: 0.4` on soft photos |
| Refined monochrome print | `{ preset: "editorial-print" }` |
| Filmic stipple with no grid | `{ preset: "film-stipple" }` |
| Risograph poster | `{ preset: "risograph" }`, `"risograph-red"` or `"risograph-duo"` |
| Comic or zine halftone, as vector dots | `{ preset: "halftone-zine", maxDim: 240 }`, then `toSVG({ svgShape: "circle" })` |
| Manga screentone | `{ preset: "manga" }`; tune `inkBelow`, `inkLines`, `paperAbove` |
| Game Boy or pixel art | `{ preset: "gameboy", pixelSize: 3 }` or `"pico8"` |
| CRT or terminal glow | `{ preset: "crt-amber" }` or `"phosphor"` |
| Exact brand colors | `{ algorithm: "atkinson", colors: ["#0f172a", "#2563eb", "#f5efe6"] }` |
| Keep the photo's own colors | `{ preset: "lo-fi-photo" }` or `{ autoPalette: 8 }` |
| Neon hero background, no input | `generate("mesh", { preset: "vapor-mesh", canvasWidth: 1200, canvasHeight: 630 })` |
| Dithered orb | `generate("sphere", { preset: "risograph", stops: ["#0b1020", "#5b3cc4", "#f7d488"] })` |

Render at the final display size (`maxDim`, or `width`/`height` with `fit`) and never scale the result afterwards: dither patterns are per pixel. Recipes are plain JSON, so a look tuned in the CLI with `--emit-recipe` drops straight in as `{ recipe }`.

## Loading

The WebAssembly loads on the first call. `init()` loads it earlier, or from somewhere else:

```js
await init();                              // now, from next to dist/index.js
await init({ wasm: "/static/dith.wasm" }); // a URL you host (bundlers that do not handle new URL(..., import.meta.url))
await init({ wasm: bytes });               // ArrayBuffer, Uint8Array, Response or a compiled WebAssembly.Module
await init({ decoders: true });            // browsers: decode images exactly as the CLI does
```

Call it before the first `dither()`; options passed after the module has loaded are an error, not silently ignored. Vite picks up the .wasm files on its own through `new URL("./dith.wasm", import.meta.url)` (the browser tests run through it, and `vite build` emits both files); webpack 5 handles the same pattern but is not tested here. With esbuild or another bundler that does not, copy `dith/dith.wasm` (or `dith/dith-decode.wasm`) to your assets and pass its URL. On Cloudflare Workers the `workerd` export condition imports `dith-decode.wasm` as a module, so wrangler bundles it with no configuration.

## Runtimes

| Runtime | Module | Encoded images are decoded by | Checked in CI |
| --- | --- | --- | --- |
| Browsers and web workers | `dith.wasm` | The browser (`createImageBitmap`), or the CLI's decoders with `init({ decoders: true })` | Chromium, through Vite and from plain `<script type="module">` |
| Node ≥ 20.16, Bun | `dith-decode.wasm` | WebAssembly: PNG, JPEG, GIF, WebP | Node 24, Bun |
| Deno | `dith-decode.wasm` | WebAssembly; needs `--allow-read` for local files and the .wasm | Deno 2 |
| Cloudflare Workers | `dith-decode.wasm` | WebAssembly | workerd, through Miniflare |

The two modules are the same engine; `dith-decode.wasm` adds dith-core's own PNG, JPEG, GIF and WebP decoders. Browsers default to the lighter one because they decode images themselves, but browser decoding is not the CLI's: JPEG decoders differ between browsers and from dith's, and translucent pixels lose precision through the canvas (premultiplied alpha). Opaque PNG, GIF and WebP decode identically. When exact CLI parity on JPEG or alpha matters in a browser, use `init({ decoders: true })`. Outside browsers there is no fallback, so other formats (AVIF, BMP, SVG) fail with `INVALID_ARGUMENT`; decode them yourself and pass pixels.

## Size

| File | Raw | gzip -9 | brotli -q 11 |
| --- | ---: | ---: | ---: |
| JS (`dist/index.js` and its chunk), as shipped | 36.2 KB | 11.7 KB | 10.3 KB |
| JS, minified | 17.2 KB | 6.8 KB | 6.1 KB |
| `dith.wasm` (browsers) | 825.7 KB | 337.2 KB | 266.1 KB |
| `dith-decode.wasm` (Node, Bun, Deno, Workers) | 1,253.2 KB | 493.9 KB | 381.5 KB |

A runtime downloads one of the two modules. `bun run size` prints this table for the current build.

## Develop

```bash
cd packages/dith
bun install
bun run wasm       # build dith.wasm and dith-decode.wasm (needs what crates/dith-wasm/README.md lists)
bun run gen        # regenerate src/generated.ts and the options tables from `dith capabilities`
bun run build      # tsdown: dist/ (ESM and .d.ts)
bun run fixtures   # CLI reference outputs for the tests (needs target/release/dith)
bun run test && bun run test:bun && bun run test:browser && bun run test:deno && bun run test:workers && bun run test:cdn
```

The tests compare every runtime's output with SHA-256 hashes of what the native CLI writes for the same cases, so a pass means byte-identical output across runtimes and with the CLI. They include the paths that use `powf`, `exp`, `sin` and `cos` (gamma, saturation, grayscale, blur, resizing, adaptive palettes, Ostromoukhov and the curved generators), which dith-core computes with its own deterministic `dith_core::detmath`, so native and WebAssembly agree there to the bit.
