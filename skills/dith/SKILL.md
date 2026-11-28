---
name: dith
description: >-
  Craft deterministic dithered images, vector SVGs, procedural art, and looping animations with the dith CLI.
  Use when an agent needs to dither photos, logos, or other assets; generate procedural dither graphics (hero backgrounds, orbs, mesh gradients, textures) with no input image;
  animate GIFs, APNGs, spritesheets, film-boil loops, dissolves, or palette cycling; make 1-bit, Risograph, halftone, Game Boy, PICO-8, CRT, or other retro/print looks;
  apply exact brand palettes; build algorithm/preset/palette comparison contact sheets; save reusable local presets or replayable JSON recipes;
  render Braille/ASCII/Kitty terminal previews; batch-dither many files; draw custom characters and icons from
  hand-written SVG shapes (inflate generator, exact ink overlays, per-frame timing, flicker-free locked-diff blinks);
  or prove what changed between two images or animation frames (dith diff).
---

# Dith

`dith` is a deterministic dithering CLI built for agents. Every command takes `--json`, writes palette-exact output, and returns a machine-readable report. The same inputs and flags always produce byte-identical files.

## Setup

1. Check for the binary with `dith --version`.
   - Inside the dith source checkout without a global install, run it via `cargo run --release -- <args>` (or `mbx run -- <args>` when `mbx` is available).
   - If dith is missing, tell the user and offer an install command from [references/cli.md](references/cli.md#install-update-or-run-from-source). Wait for their approval before installing anything.
2. Do not rely on memorized catalogs when the exact set matters. Ask the binary:
   - `dith list presets|algorithms|palettes|generators|formats --json`, filterable with `--tag <substr>` or `--category <name>`
   - `dith capabilities` for the full manifest of commands, parameter ranges, and the default recipe (about 40 KB, so prefer `list` when one catalog is enough)

## Core workflow

1. **Understand the source.** For an existing image, run `dith inspect <image> --json`. It reports dimensions, transparency, luminance percentiles (`p5`/`p50`/`p95`), `dynamic_range`, dominant colors, and preset recommendations with ready-to-run commands.
2. **Pick a starting point** from the [style guide](#choose-a-look) below. When the direction is open, render a contact sheet and look at it before committing:
   `dith compare <image> -o sheet.png --presets mac-classic,editorial-print,risograph,film-stipple --json`
3. **Validate complex flag sets first** with `--dry-run --json`. This catches bad values and shows planned `output_dimensions` without writing files.
4. **Render** with `apply`, `generate`, `animate`, or `batch`, always with `--json`. Single-file commands overwrite existing outputs by default. Add `--no-clobber` when existing files must not be touched; in `batch` it applies to each file.
5. **Verify before reporting success.** See [Verify the result](#verify-the-result).
6. **Make it reproducible** when the look will be reused. Pass `--emit-recipe` (adds a `recipe` object to the report) or `--save-preset <name>` (writes `.dith/presets/<name>.json`).

## Choose a look

Start from a preset, then override individual flags. Explicit flags always win over the preset.

| Goal | Start with | Notes |
| --- | --- | --- |
| Classic 1-bit Mac / editorial portrait | `--preset mac-classic` (atkinson) | Atkinson drops 2/8 of the error, so highlights stay airy. Add `--sharpen 0.4` on soft photos. |
| Refined monochrome print / newspaper | `--preset editorial-print` (ostromoukhov) | Smoothest gradients of the error-diffusion family. |
| Filmic stipple, no grid artifacts | `--preset film-stipple` (blue-noise) | Best general modern look; also stable in animation. |
| Risograph poster | `--preset risograph`, `risograph-red`, `risograph-duo` | Duotone and two-drum overprint looks on warm paper. |
| Comic / zine halftone | `--preset halftone-zine` | Pair with `--format svg --svg-shape circle` for vector dots. |
| Manga / screentone from an illustration | `--preset manga` | Solid black outlines and shadows, halftone mid-tones, clean white highlights. Render at the final size and check a 1:1 crop. Tune with `--ink-below`, `--ink-lines`, `--paper-above`. |
| Game Boy / pixel art | `--preset gameboy`, `pico8` | Ordered Bayer plus chunky `--pixel-size`. |
| CRT / terminal glow | `--preset crt-amber`, `phosphor` | Dark-key images suit these best. |
| Engraving / woodcut | `--preset woodcut`, `--algorithm crosshatch` | Line screens; tune with `--pattern-scale`. |
| Blueprint / technical | `--preset blueprint` | Prussian blue and chalk duotone. |
| Keep the photo's own colors | `--preset lo-fi-photo` or `--auto-palette N` | Extracts N Oklab colors (2–64) from the image. |
| Exact brand colors | `--colors "#0f172a,#2563eb,#f5efe6"` | Combine with any algorithm or preset. |
| Neon hero gradients / dark UI | `--preset vapor-mesh` with `generate mesh` | |

[references/catalog.md](references/catalog.md) has every preset, palette, algorithm, and generator with when to use each.

## Draw a custom character or icon

Build mascots and icons from hand-written SVGs with dith alone, never an image generator:

1. **Body**: draw the silhouette's parts as separate filled SVG elements (head, ears, tail) and run `dith generate inflate --shape body.svg --blend 24 --angle 135 --ground-shadow`. Each top-level element is one shape; `--blend` melts them into one soft body, and the surface is lit like `sphere` (`--depth`, `--specular`, `--rim`, `--ambient`). Shapes stretch to the canvas, so give the SVG the canvas's aspect ratio. For a light, airy drawing raise `--ambient` (0.4..0.6) and lower `--depth` (0.6..0.8).
2. **Soft tones** that should stay dithered (blush, patches, stripes): `--paint paint.svg`, where darker paint darkens the lit surface. A blurred, semi-transparent ellipse makes a blush.
3. **Line work** (eyes, nose, mouth, outline): `--overlay face.svg`, repeatable. Overlays are composited after dithering as exact pixels in the nearest palette color, cut at 50% alpha, and snapped to the `--pixel-size` grid. Draw an outline as the silhouette dilated minus the silhouette (a mask), so it never crosses the face.
4. **Animate** a blink or expression with `dith animate inflate ... --cycles 0 --overlay-frames open.svg,half.svg,closed.svg,half.svg --timings blink.json --temporal locked-diff`, then check it with `dith diff --frames out.gif --loop --json`.

[references/recipes.md](references/recipes.md#custom-character-icon) has the full cat-icon recipe; the repository's `examples/cat-icon/` builds it end to end.

## Craft rules

- **Tone before dithering.** Dithering amplifies flat or muddy input.
  - Low `dynamic_range` (under about 0.5) or a narrow p5–p95 spread: add `--auto-levels` or `--contrast 0.1..0.3`.
  - Soft photo going to 1-bit or Atkinson: `--sharpen 0.3..0.6`.
  - Dark midtones: `--gamma 0.7..0.9` (values under 1 brighten). Bright, washed-out input: `--gamma 1.2..1.5`.
  - Nudging overall density: `--bias` (negative is darker, positive is lighter, range ±0.5).
- **Size first.** Cap with `--max-dim` (or `--width`/`--height` plus `--fit contain|cover|stretch`). Dither patterns are per-pixel, so a dithered image must never be resized afterwards. Render at the final display size instead.
- **Chunky pixels.** `--pixel-size N` downsamples, dithers, then nearest-upscales, so output dimensions are unchanged.
- **Solid ink and clean paper.** Any algorithm dithers every tone into dots. For print looks (manga, comics, zines, woodcut, Risograph) add `--ink-below 0.15..0.3` so deep shadows print as solid ink, `--paper-above 0.85..0.95` so skies and highlights stay clean paper, and `--ink-lines 0.3..0.8` to trace the source's outlines in solid ink (higher picks up fainter lines). Ink is the darkest palette color and paper the lightest. Outlines need line art at a decent resolution; they are tuned for images around 3000 px wide and scale with size.
- **Ordered screen size.** `--pattern-scale 2..4` enlarges Bayer, halftone, line, and blue-noise cells for bolder, more graphic texture.
- **Color space.** Keep the default `oklab`. Use `srgb` only to mimic legacy hardware math; `linear` is physically linear light.
- **Determinism.** Stochastic algorithms (`blue-noise`, `random`, `ign`), `--noise`, and `--auto-palette` use `--seed` (default 42). Change the seed for variations and keep it fixed for reproducibility.
- **SVG input.** Any input can be an `.svg`. It is rasterized at the target size (`--width`, `--height`, `--max-dim`, or its own width/height/viewBox), keeps its alpha, and renders identically everywhere. Text elements are not drawn; convert text to paths.
- **Transparency.** Logos and icons with alpha: `--alpha-mode preserve` keeps the alpha channel, `threshold` gives a crisp 1-bit mask, `dither` gives a stippled fade, and `flatten --background "#hex"` composites onto a color.
- **Turning off preset parts.** Most monochrome presets turn on `--grayscale` and `--auto-levels`. Use `--no-grayscale`, `--no-auto-levels`, and `--no-serpentine` to disable what a preset turned on.

## Output formats

The format is inferred from the `-o` extension; pass `--format` to override it.

- **PNG** (default): lossless and pixel-exact, saved as an indexed PNG at the smallest bit depth the palette allows. The right choice almost always; only lossless WebP is smaller. Add `--optimize` when file size matters more than encode time: it searches palette orders and PNG filters and compresses with Zopfli, giving PNG, APNG, and spritesheets that are typically 3–25% smaller with identical pixels, at several times the encode time (seconds per image).
- **BMP / TIFF**: lossless, for tools that require them. BMP is palettized at 1, 4, or 8 bits when the image is opaque with at most 256 colors (else 24- or 32-bit); TIFF is Deflate-compressed. Both are still larger than PNG.
- **WebP**: lossless only, palette-indexed, and typically 2–20% smaller than dith's PNG for dithered output (biggest gains on ordered dithers and 3+ color palettes), but slower to encode: up to about a second for a 1200×800 image with more than 16 colors. **JPEG** destroys dither patterns, so never use it for dithered output unless the user insists.
- **SVG**: `--svg-shape rect` (run-length merged rectangles, smallest), `circle` (halftone dots), or `dot` (inset dots). `--svg-skip-bg` drops the dominant background color for overlays. SVG grows with pixel count, and circles are much larger than rects, so keep `--max-dim` around 120–320.
- **Terminal text**: `--format braille|ascii` with `--text-width N`. `-o` is optional and the text goes to stdout.
- **Stdin/stdout**: pass `-` as input or as `-o -`.

## Animation

`dith animate` takes one of four kinds of input:
- a pattern name (`sphere`, `waves`, `mesh`, `plasma`, ...) for a seamless procedural loop. Each pattern moves differently (the sphere stays still while its light orbits, for example); the `motion` field of `dith list generators --json` describes it. To check the motion cheaply before a full render, export a small spritesheet (`--anim-format spritesheet --sheet-cols <frames> --canvas-width 160 --canvas-height 120`) and look at it.
- an animated GIF, APNG, or WebP to re-dither
- several image files as a frame sequence
- a single static image, which gets film boil (`--temporal boil` is the default for this case)

Output goes to `.gif` or `.apng` (both exact-palette and delta-encoded; APNG also keeps partial transparency and is often a little smaller), a spritesheet `.png` via `--anim-format spritesheet --sheet-cols N`, or numbered PNG frames via `--frames-dir DIR`.

- **Flicker.** `--temporal locked` (the default) fixes the screen and seed across frames, but it only makes static regions rock-solid with per-pixel algorithms: ordered screens (`bayer*`, `halftone`, line screens), `blue-noise`, `ign`, `random`, and `threshold`. Error diffusion (`floyd-steinberg`, `atkinson`, `stucki`, `riemersma`, ...) passes every change on to neighbouring pixels, so anything that moves makes the stipple shimmer below and to the right of it. Atkinson shimmers least. The `temporally_stable` field of `dith list algorithms --json` says which is which. `--temporal locked-diff` fixes that for error diffusion: each frame only takes pixels within `--diff-margin` (default 4 px) of a source change and copies the rest from the previous frame, so a blink stays a blink. `boil` gives a hand-drawn shimmer on purpose; set `--boil-rate 2` to animate "on twos". `grain` re-randomizes stipple every frame.
- **Seamless loops.** Procedural patterns loop by construction. For sweeps, use `--sweep-mode pulse` or `--ping-pong`.
- **Sweeps** animate a parameter over time: `--sweep-bias`, `--sweep-brightness`, `--sweep-contrast`, `--sweep-gamma`, `--sweep-strength`, `--sweep-pixel-size`, each written `START..END`. Shape them with `--easing`, and pause on keyframes with `--hold-start` and `--hold-end`.
- **Palette.** One shared global palette is the default and keeps colors steady. Use `--per-frame-palette` only for footage whose colors change a lot.
- **Per-frame timing.** Hold one frame without repeating the file: `dith animate open.png:4200ms half.png:50ms shut.png:100ms half.png:50ms` (or `:84f` for 84 frames at `--fps`), or `--timings times.json` with one entry per frame (`[4200, 50, "2f", 50]`; works for every source). `--hold-start`/`--hold-end` add extra copies of the first/last frame, each at that frame's own delay, for any source including frame sequences. The report lists every frame's `delay_ms`, and for GIF its `encoded_delay_ms` plus a `timing_note` when a delay was rounded.
- **Timing.** Procedural and boil frames are capped at 2..=240. `--duration S` is an alternative to `--frames` (frames = S × fps); passing both is an error. GIF delays are stored in 10 ms steps, so `--fps 10|20|25|50` gives exact GIF timing (15 fps becomes 70 ms per frame). APNG is exact.
- **Size.** File size grows with frames × pixels, and dither noise barely compresses.
  - GIFs and APNGs are delta-encoded automatically: each frame after the first only stores what changed. Locked per-pixel algorithms with static areas are therefore far smaller than error diffusion or `boil`/`grain`, where nearly every pixel changes each frame.
  - To hit a budget, pass `--max-size 2MB` (also `500KB`, `1.5MiB`, or bytes). Dith shrinks the frame dimensions, keeping frame count and fps, until the file fits, and reports `size_fit_scale` (1.0 = untouched). If `size_fit_scale` comes back small (under about 0.5), the result may be too small to use. Lower the size another way instead: fewer frames, a locked ordered algorithm, or fewer palette colors.
  - Keep procedural canvases modest (defaults are 480×360).
- **Procedural colors.** Without `--stops`, patterns use a plain near-black → near-white ramp. Pass `--stops` to choose the source tones deliberately; it usually looks better, especially with a colored palette.
- **Presets on procedural sources.** Presets are tuned for photos. For `generate`, `animate <pattern>`, and `compare`/`preview --pattern`, dith skips their auto-levels, contrast, and sharpen, because those clip clean gradients into flat patches. Pass those flags explicitly to apply them anyway.

## Verify the result

Read the JSON report rather than assuming success:

- `"status": "ok"`. `batch` can instead return `"partial_failure"` with exit code 1; check `failed` and each `items[].error`.
- `output_dimensions` for images, or `frame_count`, `frame_dimensions`, and `total_duration_ms` for animations, match the intent.
- `bytes_written > 0`; it is `0` on `--dry-run`.
- `palette_colors` and `color_usage` are as expected. If one color covers more than about 95% of pixels, the image is nearly blank: adjust `--bias`, `--gamma`, or `--contrast`, or try `--invert`.

To prove what changed, run `dith diff a.png b.png --json` (changed pixels, percentage, `bbox`, `changed_rows`/`changed_columns` runs, `-o` writes a highlight PNG) or `dith diff --frames anim.gif --loop --json`, whose `union` covers every pixel that changes in any frame. Every animate report also has `frames[].changed_pixels` and `changed_pixels_total`.

When aesthetics matter, look at the output. Open PNG files with an image-viewing tool if you have one; otherwise add `--preview` to get a Braille thumbnail in `preview_braille`. For animations, export a spritesheet or `--frames-dir` and view the PNG frames. For SVGs, render a PNG with the same flags and view that.

## Errors

With `--json`, errors go to stderr as `{"status":"error","code","message","suggestion","valid_values"?}`.

- **Exit 2**: invalid input from you. Codes include `CLI_USAGE_ERROR`, `INVALID_ARGUMENT` (an out-of-range value; the message includes the range), `UNKNOWN_PRESET` and similar codes (`valid_values` lists the options), and `OUTPUT_EXISTS` (`--no-clobber` hit an existing file), and `INVALID_ASSET` (an `--overlay`, `--shape` or `--paint` file is missing or not an image). Fix the flags and retry.
- **Exit 1**: runtime or I/O failure (`RUNTIME_ERROR`), such as a missing input or an undecodable file. `batch` also exits 1 when any item fails.

## Use from JavaScript

When the work runs in a browser, Node, Bun, Deno or a Cloudflare Worker instead of a shell, use the `dith` package in the repository's `packages/dith` (WebAssembly, zero dependencies, not on npm yet). Options are the CLI flags in camelCase, and output is byte-identical to the CLI's:

```js
import { dither, generate } from "dith";
const out = await dither("photo.jpg", { preset: "risograph", maxDim: 960 }); // path, URL, bytes, Blob, ImageData, <img>, canvas
out.toPNG();      // the bytes of: dith apply photo.jpg --preset risograph --max-dim 960
out.recipe;       // the --emit-recipe JSON, for --recipe in the CLI
const orb = await generate("sphere", { preset: "risograph", canvasWidth: 800, canvasHeight: 600 });
```

Errors are `DithError` with the CLI envelope's `code`, `suggestion` and `validValues`. [packages/dith/llms.txt](../../packages/dith/llms.txt) has the whole API on one page. Animation, batch, compare, overlays and other file-path features are CLI-only.

## Interactive vs headless

`--view`, `preview` (fullscreen by default), and `compare --view` are interactive Kitty-protocol viewers meant for a human at a terminal. In non-interactive agent shells, render to files, use `--preview` or `--format braille`, or pass `--inline` (for example `dith preview img.png --inline --mode braille --cols 60`). Only use the fullscreen viewer when the user explicitly wants to watch in their own terminal.

## References

- [references/cli.md](references/cli.md): every subcommand and flag, with ranges, defaults, and JSON report fields.
- [references/catalog.md](references/catalog.md): presets, palettes, algorithms, and generators, and when to choose each.
- [references/recipes.md](references/recipes.md): copy-ready recipes for common jobs (web hero, avatar, logo SVG, social card, CSS spritesheet, reveal GIF, custom character icon, brand preset, batch export).
