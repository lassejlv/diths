# Dith CLI reference

Every subcommand accepts `--json`. `dith <command> --help` is always authoritative. `dith capabilities` dumps all commands, parameter ranges, and the default recipe as JSON.

## Contents

- [Install, update, or run from source](#install-update-or-run-from-source)
- [Discovery: list, capabilities, inspect](#discovery)
- [Shared dither flags](#shared-dither-flags) (apply, generate, animate, batch, compare, preview, preset save)
- [apply](#apply)
- [generate](#generate)
- [animate](#animate)
- [compare](#compare)
- [batch](#batch)
- [preview](#preview)
- [preset](#preset)
- [diff](#diff)
- [Ink overlays](#ink-overlays) (apply, generate, animate)
- [JSON reports](#json-reports)
- [Errors and exit codes](#errors-and-exit-codes)
- [Use from JavaScript](#use-from-javascript)

## Install, update, or run from source

```sh
# Prebuilt binary + agent skill (macOS Apple Silicon, Linux x86_64, Linux aarch64).
# Asks before copying the skill into each detected agent; add `-s -- --yes` to skip the prompt.
curl -fsSL https://cdn.diths.design/install.sh | bash

# Self-update from GitHub Releases (also refreshes the skill in detected agents)
dith update --check --json     # only report whether a newer release exists
dith update                    # install latest; --version v0.2.0 to pin, --force to reinstall, --no-skills to skip skills

# From source (any platform with Rust)
cargo install --git https://github.com/lassejlv/diths

# Inside the dith source checkout
cargo run --release -- --help
```

`install.sh` options:
- `--version <tag>`
- `--dir <path>` (default `~/.local/bin`)
- `--no-skills`
- `--yes`

The same settings work as environment variables: `DITH_VERSION`, `DITH_INSTALL_DIR`, `DITH_NO_SKILLS=1`, `DITH_YES=1`.

## Discovery

```sh
dith capabilities                                     # full manifest (~40 KB JSON)
dith list presets --json                              # built-in + local presets with full recipes
dith list algorithms --category error-diffusion --json   # categories: error-diffusion, ordered, stochastic, baseline
dith list palettes --tag print --json                 # tag substring, case-insensitive
dith list generators --json
dith list formats --json
dith inspect photo.png --colors 6 --json              # --colors 2..=16 dominant colors
```

`inspect` reports:
- `width`, `height`, `aspect_ratio`, `color_type`
- `has_transparency`, `transparent_percent`, `is_grayscale`
- `mean_luminance`, `dynamic_range`, `p5_luminance`, `p50_luminance`, `p95_luminance` (all 0..1)
- `dominant_colors`
- `recommendations[]`, each with `style`, `preset`, `rationale`, and `example_command`

## Shared dither flags

These flags apply to `apply`, `generate`, `animate`, `batch`, `preview`, `preset save`, and to `compare` (where they act as the base for every tile).

The resolution order is the recipe (`--recipe <path|->`), then the preset (`-P/--preset <id>`), then explicit flags. Each later layer overrides the one before it.

**Algorithm and diffusion**

| Flag | Range / values | Default |
| --- | --- | --- |
| `-a, --algorithm` | see [catalog](catalog.md#algorithms) | `floyd-steinberg` |
| `--color-space` | `oklab`, `linear`, `srgb` | `oklab` |
| `--strength` | 0.0..=1.5 (0 posterizes, >1 exaggerates) | 1.0 |
| `--serpentine` / `--no-serpentine` | error diffusion scan direction | serpentine on |
| `--bias` | -0.5..=0.5 (negative darker, positive lighter) | 0.0 |
| `--noise` | 0.0..=0.5 deterministic jitter that breaks banding and grids | 0.0 |
| `--pattern-scale` | 1..=32 cell size for ordered, halftone, line, and blue-noise screens | 1 |
| `--seed` | u64 for blue-noise, random, ign, noise, and auto-palette | 42 |

**Solid ink** (works with every algorithm; all off by default)

| Flag | Range | Effect |
| --- | --- | --- |
| `--ink-below` | 0.0..=1.0 (0 = off) | Tones darker than this luminance become solid ink (the darkest palette color) instead of dots. |
| `--paper-above` | 0.0..=1.0 (1 = off) | Tones lighter than this luminance stay clean paper (the lightest palette color). |
| `--ink-lines` | 0.0..=1.0 (0 = off) | Finds outlines (pixels clearly darker than their blurred surroundings) and draws them as solid ink. Higher values pick up fainter lines. |

Ink and paper are picked by Oklab lightness, so they also work with multi-color palettes. The masks come from the prepared source after resize and tone, so they are deterministic and hold still across animation frames in any `--temporal` mode. Recipes store them as `dither.ink_below`, `dither.paper_above` and `dither.ink_lines`, omitted when unset. With `compare --presets`, these three flags also override every preset tile.

**Palette**

Choose one source.

| Flag | Notes |
| --- | --- |
| `-p, --palette <id>` | Built-in palette, or `grayN` for N in 2..=64. Default `bw`. |
| `--colors "#a,#b,..."` | Exact hex list. |
| `--palette-file <path>` | `.hex`/`.txt` (one hex per line), `.gpl` (GIMP), or `.json`. |
| `--auto-palette <N>` | 2..=64 colors extracted from the image with Oklab k-means++. |

**Resize** (applied before dithering)

| Flag | Notes |
| --- | --- |
| `--width`, `--height` | Target size. With both set, `--fit contain\|cover\|stretch` applies (default `contain`). |
| `--max-dim <N>` | Caps the longest side, keeping the aspect ratio. |
| `--scale <f>` | Uniform multiplier. |
| `--pixel-size <N>` | 1..=64. Downscale by N, dither, then nearest-upscale. Output dimensions are unchanged. |

**Tone** (applied before dithering)

| Flag | Range |
| --- | --- |
| `--brightness` | -1.0..=1.0 |
| `--contrast` | -1.0..=1.0 |
| `--gamma` | 0.1..=5.0 (<1 brightens midtones) |
| `--saturation` | 0.0..=3.0 (0 is gray, 1 is neutral) |
| `--sharpen` | 0.0..=5.0 unsharp mask |
| `--blur` | 0.0..=20.0 Gaussian sigma |
| `--auto-levels` / `--no-auto-levels` | 1st–99th percentile stretch |
| `--grayscale` / `--no-grayscale` | Perceptual grayscale |
| `--invert` | Invert tones |

**Alpha**

- `--alpha-mode preserve|flatten|threshold|dither` (default `preserve`)
  - `preserve` keeps alpha, and transparent pixels do not diffuse error.
  - `flatten` composites onto `--background "#hex"` (default `#ffffff`).
  - `threshold` cuts alpha at 50%.
  - `dither` stipples alpha with Bayer 4×4.

**Output**

- `-f, --format png|webp|jpeg|gif|bmp|tiff|svg|kitty|braille|ascii`, inferred from the `-o` extension. Stdout (`-o -`) defaults to PNG.
- SVG: `--svg-shape rect|circle|dot`, `--svg-skip-bg`
- `--optimize`: smallest lossless PNG, APNG, spritesheet, and PNG frames (palette order and filter search plus Zopfli), and JPEG with Huffman tables fitted to the image. Pixels are identical; PNG encoding takes several times longer. Recorded in recipes as `export.optimize`.
- Text: `--text-width <cols>` for braille and ascii
- Kitty: `--view-cols`, `--view-rows`

## apply

```sh
dith apply <INPUT|-> [-o <PATH|->] [shared flags] [options]
```

| Option | Notes |
| --- | --- |
| `-o, --output` | File path, or `-` for stdout. Optional for braille, ascii, and kitty (printed to stdout). |
| `--no-clobber` | Fail with `OUTPUT_EXISTS` instead of overwriting. Overwriting is the default. |
| `--dry-run` | Validate and plan without writing (`bytes_written: 0`). Includes `recipe`. |
| `--emit-recipe` | Add the fully resolved `recipe` to the report (printed after the summary without `--json`). |
| `--save-preset <name>` | Also save the resolved settings as a preset. `--preset-scope project\|user` (default `project`). The report gains `saved_preset_path`. |
| `--preview` | Add a Braille thumbnail as `preview_braille` in the report. |
| `--view` / `-F` / `--inline` | Kitty-protocol terminal viewer, for interactive TTYs only. |

Inputs can be PNG, JPEG, WebP, GIF (first frame), BMP, TIFF, or SVG. An SVG is rasterized at the target size (`--width`/`--height`/`--max-dim`, or its own width/height/viewBox) with its alpha kept, so `--alpha-mode` works as for PNG. SVG text is not drawn; convert it to paths. SVG input works the same in `generate`/`animate` frame lists, `batch`, `compare`, `preview`, `inspect` and `diff`.

`apply` also takes the [ink overlay](#ink-overlays) flags.

## generate

```sh
dith generate <PATTERN> -o <PATH|-> [shared flags] [options]
```

| Option | Default |
| --- | --- |
| `--canvas-width`, `--canvas-height` | 640, 480 |
| `--angle <deg>` | 135 |
| `--frequency <f>` | 1.0 |
| `--stops "#a,#b,..."` | Pattern default. Source colors interpolated in Oklab. |

It also accepts `--no-clobber`, `--dry-run`, `--emit-recipe`, `--save-preset`/`--preset-scope`, `--preview`, and `--view`/`--inline`. The patterns are listed in the [catalog](catalog.md#generators).

**inflate** puffs your own shape up like a pillow and lights it like `sphere`:

| Option | Range / default | Notes |
| --- | --- | --- |
| `--shape <svg\|png>` | required, repeatable | Each top-level SVG element (or `<g>`) is one shape; a PNG is one shape. Stretched to the canvas. |
| `--shape-channel` | `alpha` (default), `luminance`, `darkness` | What marks the filled area. |
| `--blend <px>` | 0..=512, 0 | Smooth-union width that melts shapes into one body. |
| `--depth` | 0.05..=4.0, 1.0 | 1.0 inflates a disc to a hemisphere. |
| `--specular`, `--rim` | 0.0..=2.0, 0.42 and 0.28 | Highlight and rim light, as on `sphere`. |
| `--ambient` | 0.0..=1.0, 0.08 | Floor brightness; raise it for light, airy drawings. |
| `--ground-shadow [darkness]` | 0.0..=1.0, 0.35 when given | Soft ellipse under the shape. |
| `--paint <svg\|png>` | none | Surface tone: darker paint darkens the lit surface (blush, patches). Transparent = unpainted. |

The background is the lightest `--stops` color. `--angle` sets the light direction (135 = top left). `animate inflate` orbits the light like `sphere`; `--cycles 0` keeps it still.

`generate` also takes the [ink overlay](#ink-overlays) flags.

With `--preset`, procedural sources skip the preset's photo preparation (auto-levels, contrast, sharpen) unless you pass those flags explicitly. This also applies to `animate <pattern>`, `compare --pattern`, and `preview --pattern`.

## animate

```sh
dith animate <PATTERN | INPUT.gif|.apng|.webp | FRAME1 FRAME2 ... | STILL.png> -o <out.gif|out.apng|sheet.png> [shared flags] [options]
```

You must pass `-o`, `--frames-dir`, or `--view`. `--pattern <name>` forces procedural mode explicitly.

**Container**

| Option | Notes |
| --- | --- |
| `--anim-format gif\|apng\|spritesheet\|frames` | Inferred from `-o`. A `.png` output needs `spritesheet`. |
| `--sheet-cols <N>` | Spritesheet columns. Use `N = frames` for a single row. The report has `spritesheet_grid: [cols, rows]`. |
| `--frames-dir <DIR>` | Also (or only) write `frame_0001.png`, ... |
| `--loops <N>` | 0 = infinite (default). |
| `--max-size <SIZE>` | File-size budget such as `2MB`, `500KB`, `1.5MiB`, or bytes (KB/MB are 1000-based). Re-renders at smaller frame dimensions, keeping frames and fps, until the output fits. Needs `-o` with gif, apng, or spritesheet output. Fails with `INVALID_ARGUMENT` if it can't fit even at 8 px. The report has `max_size_bytes` and `size_fit_scale`. |

**Timing**

| Option | Notes |
| --- | --- |
| `--frames <N>` | 2..=240, default 24 (procedural and still-photo boil). |
| `--duration <s>` | Mutually exclusive with `--frames`. Frames = s × fps. |
| `--fps <N>` | 1..=60, default 15. Overrides source GIF timing only when passed. GIF delays round to 10 ms. |
| `--max-frames <N>` | Cap decoded frames from input GIFs. |
| `FRAME.png:4200ms`, `FRAME.png:84f` | Per-frame time in a frame sequence: milliseconds, or frames at `--fps`. 10..=65535 ms. Untimed frames use `--fps`. |
| `--timings <json>` | One time per frame (before ping-pong and holds), e.g. `[4200, 50, "100ms", "1f"]`. Works for every source; not together with `file:time` inputs. |
| `--reverse`, `--ping-pong` | Ping-pong appends the reversed interior frames. |
| `--hold-start <N>`, `--hold-end <N>` | Extra copies of the first/last frame, each at that frame's own delay, for every source including frame sequences. Added to `frame_count`. To hold one frame for a long time, give it a long time instead. |

APNG stores every delay exactly. GIF stores 10 ms steps with a 20 ms minimum; the report then shows each frame's `encoded_delay_ms` and a `timing_note`.

**Temporal dithering**

| Option | Notes |
| --- | --- |
| `--temporal locked\|boil\|grain\|locked-diff` | Default `locked`, or `boil` for a single still input. `locked` fixes screen and seed; static pixels stay identical only with per-pixel algorithms (`temporally_stable: true` in `dith list algorithms --json`). Error diffusion still shimmers around motion, unless you use `locked-diff`: every frame is dithered in full, but only pixels near a source change are taken from it and the rest copy the previous output frame. |
| `--diff-threshold <f>` | `locked-diff` only. Source luminance change (0.0..=1.0) that counts; default 0 = any change. |
| `--diff-margin <px>` | `locked-diff` only. Grows the changed area (0..=256, default 4), rounded up to whole `--pixel-size` cells. |
| `--boil-rate <N>` | Update the boil every N frames (2 = on twos). Default 1. |
| `--boil-amount <f>` | 0.0..=0.5 jitter intensity. |
| `--screen-drift <dx,dy>` | Per-frame drift of ordered or halftone screens, in cells. |
| `--per-frame-palette` | Adaptive palette per frame instead of one shared palette. |
| `--cycle-palette` | VGA/Amiga-style palette color cycling. |

**Motion and sweeps**

| Option | Notes |
| --- | --- |
| `--easing linear\|ease-in-out\|ease-in\|ease-out\|smoothstep\|bounce\|steps` | `--easing-steps <N>` (default 6) applies to `steps`. |
| `--cycles <f>` | Motion cycles per loop (default 1.0). |
| `--phase-offset <0..1>` | Start phase, useful for syncing several animations. |
| `--motion-amplitude <f>` | 0 = still, 1 = default, 2 = dramatic. |
| `--sweep-mode linear\|pulse` | `pulse` goes start → end → start, so it loops seamlessly. |
| `--sweep-bias A..B` | e.g. `-0.45..0.0`, an ink-wash dissolve. |
| `--sweep-brightness A..B` | e.g. `-0.7..0.0`, a fade-in. |
| `--sweep-contrast A..B`, `--sweep-gamma A..B` | Tonal sweeps. |
| `--sweep-strength A..B` | e.g. `0.0..1.2`, posterize to full stipple. |
| `--sweep-pixel-size A..B` | e.g. `12..1`, a mosaic focus-in. |

**Procedural source**

- `--canvas-width`/`--canvas-height` (defaults 480×360), `--angle`, `--frequency`, `--stops`
- What each pattern does over time is in `dith list generators --json` (`motion`), and in the [catalog](catalog.md#generators).

**Overlays**

- [Ink overlay](#ink-overlays) flags apply to every frame.
- `--overlay-frames <dir|a.svg,b.svg,...>`: one overlay per frame (before ping-pong and holds), composited on top. A directory is read in name order. Lets eyes or lettering change while the dithered picture stays put.

**Other**

- `--no-clobber`, `--dry-run`
- `--view` with `--view-mode kitty|braille|ascii` plays live in a TTY; `--inline` prints the first frame instead.

## compare

```sh
dith compare [INPUT | --pattern <name>] -o sheet.png [--out-dir DIR] [shared flags] [options]
```

| Option | Notes |
| --- | --- |
| `--algorithms a,b,...` | One tile per algorithm. |
| `--presets a,b,...` | One tile per preset. |
| `--palettes a,b,...` | One tile per palette. With `--algorithms`, a single palette applies to every algorithm tile. |
| `--columns <N>` | Default: an automatic square-ish grid. |
| `--tile-max-dim <N>` | Default 360. |
| `--light` | Warm paper frame instead of the dark matte. |
| `--out-dir <DIR>` | Write each variant as `<label>.png`. Usable without `-o`. |
| `--dry-run`, `--view`, `--inline` | |

## batch

```sh
dith batch <FILES...> -d|--out-dir <DIR> [shared flags] [--suffix -dithered] [--no-clobber] [--dry-run]
```

- Processes files in parallel. Pass files or a shell glob; directories are not expanded.
- The output name is `<stem><suffix>.<ext>`. The default suffix is `-dithered`; use `--suffix ""` to keep names.
- The extension follows `--format`, or stays `.png` when no format is given.
- When any item fails (including files skipped by `--no-clobber`), `status` is `partial_failure` and the exit code is 1.

## preview

```sh
dith preview [INPUT|- | --pattern <name>] [shared flags] [-m kitty|braille|ascii] [--inline] [--cols N] [--rows N] [--raw]
```

- Fullscreen by default. In agent shells, use `--inline --mode braille|ascii`.
- `--raw` previews the source without dithering, for before/after comparisons.
- With `--pattern`, it also accepts `--canvas-width`, `--canvas-height`, `--angle`, `--frequency`, and `--stops`.

## preset

```sh
dith preset save <name> [shared flags] [--title T] [--description D] [--tags a,b] \
  [--scope project|user | --dir DIR] [--no-clobber] --json
dith preset list [--source all|local|project|user|builtin] [--tag t] --json
dith preset show <name> --json          # local presets first, then built-ins
dith preset delete <name> [--scope project|user | --dir DIR] --json
```

- Names use lowercase letters, digits, and hyphens.
- Storage locations:
  - project: `.dith/presets/<name>.json`
  - user: `~/.config/dith/presets/<name>.json`
- Lookup order: `$DITH_PRESETS_DIR`, then `.dith/presets/`, then `~/.config/dith/presets/`, then built-ins. A local preset with a built-in's name shadows the built-in.
- From a render, `--save-preset <name> --preset-scope project|user` on `apply` or `generate` saves the resolved settings in one step.

## diff

```sh
dith diff <A> <B> [-o highlight.png] [--tolerance N] [--dry-run] --json
dith diff --frames <anim.gif|.apng|.webp|DIR> [--loop] [-o highlight.png] [--tolerance N] --json
```

- Compares RGBA pixel by pixel; both images must be the same size (`INVALID_ARGUMENT` otherwise). Two fully transparent pixels always match.
- `--tolerance N` (0..=255) ignores per-channel differences up to N.
- `-o` writes a PNG that fades the (last) image and paints changed pixels red; with `--frames` it marks every pixel that changes in any frame.
- `--frames` compares each frame with the one before it; `--loop` also compares the last frame with the first. A directory is read in name order.
- Exit code 0 whether or not anything changed; read `changed_pixels`.

## Ink overlays

`apply`, `generate` and `animate` composite overlays after dithering, so line work stays exact:

| Option | Notes |
| --- | --- |
| `--overlay <svg\|png>` | Repeatable, composited in order. SVGs are rasterized at the output size; PNGs of another size are scaled nearest-neighbour. Every drawn pixel snaps to the nearest palette color, with no dithering and no error diffusion. |
| `--overlay-color <hex>` | Paint every drawn overlay pixel this color instead (use a palette color for GIF output). |
| `--overlay-alpha threshold\|dither` | `threshold` (default) draws pixels at least 50% opaque; `dither` screens partial alpha with a 4x4 Bayer matrix. |

Under `--pixel-size N`, whole N x N blocks switch together (from their mean opacity and color), so overlays line up with the chunky pixels. Overlays are stored in recipes as `overlay: {layers, color, alpha}` and omitted when unset; saved presets keep the inking but drop the layer files. A missing or unreadable file fails with `INVALID_ASSET` (exit 2).

## JSON reports

**apply / generate**

- `status`, `dry_run`, `source` (path, or `generate:<pattern>`), `output`, `format`
- `preset`, `algorithm`, `color_space`, `palette_name`, `palette_colors[]`
- `source_dimensions`, `working_dimensions`, `output_dimensions` (each `[w, h]`)
- `bytes_written`, `elapsed_ms`
- `color_usage[]`, each `{hex, pixels, percent}`
- Optional: `recipe`, `preview_braille`, `saved_preset_path`

**animate**

- `status`, `dry_run`, `source`, `output`, `frames_dir`, `anim_format`, `temporal`
- `frame_count`, `fps`, `total_duration_ms`, `loops` (`fps` is `null` unless every frame has the same delay and that rate is 1..=60 fps; with per-frame timing read `frames[].delay_ms` and `total_duration_ms`; the text summary then reads `4 frames, 4.40 s, variable timing`)
- `preset`, `algorithm`, `color_space`, `palette_name`, `palette_colors[]`
- `source_dimensions`, `frame_dimensions`
- `spritesheet_grid` (spritesheets only)
- `max_size_bytes`, `size_fit_scale` (only with `--max-size`)
- `bytes_written`, `elapsed_ms`, `color_usage[]`
- `frames[]`, each `{index, delay_ms, encoded_delay_ms?, changed_pixels?}` (`encoded_delay_ms` for GIF; `changed_pixels` against the previous frame, 0 for the first, absent on `--dry-run`), `changed_pixels_total`, `timing_note` (GIF rounding)

**diff**

- Two images: `status`, `mode: "images"`, `a`, `b`, `tolerance`, `width`, `height`, `changed_pixels`, `total_pixels`, `changed_percent`, `bbox` (`{x, y, width, height}` or null), `changed_rows` and `changed_columns` (inclusive `[first, last]` runs), `output`
- Frames: `mode: "frames"`, `source`, `frame_count`, `width`, `height`, `loop`, `unchanged_frames`, `changed_pixels_total`, `frames[]` (`{index, previous, delay_ms, ...}` plus the fields above), and `union` (every pixel that changes in any compared pair)

**compare**

- `status`, `dry_run`, `source`, `sheet_output`, `sheet_dimensions`, `out_dir`
- `variants[]`, each `{label, algorithm, palette, preset, file, elapsed_ms}`

**batch**

- `status` (`ok` or `partial_failure`), `dry_run`, `out_dir`, `total`, `succeeded`, `failed`
- `items[]`, each `{input, output, status, output_dimensions, bytes_written, elapsed_ms, error?}`

**update**

- `status`, `current_version`, `latest_version`, `update_available`, `updated`
- `target`, `install_path`, `release_url`
- `skills_updated`, `skills_agents[]`

## Errors and exit codes

With `--json`, errors are printed to stderr:

```json
{
  "status": "error",
  "code": "UNKNOWN_PRESET",
  "message": "unknown preset 'nope'",
  "suggestion": "Run `dith list presets --json` ...",
  "valid_values": ["blueprint", "crt-amber", "..."]
}
```

| Exit | Meaning | Typical codes |
| --- | --- | --- |
| 0 | Success | |
| 2 | Invalid flags or values. Fix and retry. | `CLI_USAGE_ERROR` (clap parse error; message lists possible values), `INVALID_ARGUMENT` (out of range, missing `-o`), `UNKNOWN_PRESET` and other unknown-slug codes (with `valid_values`), `OUTPUT_EXISTS`, `INVALID_ASSET` (overlay, shape or paint file missing or not an image) |
| 1 | Runtime or I/O failure | `RUNTIME_ERROR` (missing or undecodable input, write failure, batch item failures) |

Without `--json`, errors print as `error [CODE]: message` followed by `hint: suggestion`.

## Use from JavaScript

The `dith` package in `packages/dith` runs the same engine as WebAssembly in browsers, Node ≥ 20.16, Bun, Deno and Cloudflare Workers. Each shared dither flag is an option in camelCase (`--pixel-size 3` is `pixelSize: 3`, `--no-serpentine` is `serpentine: false`), `dith generate` flags are `generate()` options (`--canvas-width` is `canvasWidth`), and layering is the CLI's: `recipe`, then `preset`, then the other options.

| CLI | JavaScript |
| --- | --- |
| `dith apply in.jpg -o out.png --preset manga --max-dim 960` | `(await dither("in.jpg", { preset: "manga", maxDim: 960 })).toPNG()` |
| `dith generate sphere -o orb.svg --preset risograph --canvas-width 320` | `(await generate("sphere", { preset: "risograph", canvasWidth: 320 })).toSVG()` |
| `--recipe look.json` / `--emit-recipe` | `{ recipe: look }` / `result.recipe` or `await recipe(options)` |
| `dith list presets --json` | `await presets()` (also `palettes()`, `algorithms()`, `generators()`) |
| `--json` error envelope | `DithError`: `code`, `message`, `suggestion`, `validValues`; `err.toJSON()` is the envelope |

PNG, GIF and SVG bytes match the CLI's for the same input and flags. The package has no `animate`, `batch`, `compare`, `preview`, `diff`, `--palette-file`, `--overlay`, `--shape` or `--paint`. See [packages/dith/README.md](../../../packages/dith/README.md) for inputs, loading and the full option table.
