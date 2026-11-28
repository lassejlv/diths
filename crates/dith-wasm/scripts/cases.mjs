// Cases shared by parity.mjs and bench.mjs. A case dithers the input photo with a recipe, or
// draws a generator (and dithers it when it has a recipe). See examples/native.rs.

const pal = (value) => ({ mode: "builtin", value });
const rgb = (hex) => ({ r: parseInt(hex.slice(1, 3), 16), g: parseInt(hex.slice(3, 5), 16), b: parseInt(hex.slice(5, 7), 16) });

/** Every preset, every algorithm on 2- and 16-color palettes, preprocessing paths and generators. */
export function parityCases(w) {
  const cases = [];
  for (const p of w.presets()) cases.push({ id: `preset-${p.id}`, recipe: p.id });
  for (const { id } of w.algorithms()) {
    for (const palette of ["bw", "pico8"]) {
      cases.push({
        id: `algo-${id}-${palette}`,
        recipe: { preprocess: { max_dim: 640 }, palette: pal(palette), dither: { algorithm: id } },
      });
    }
  }
  for (const algorithm of ["floyd-steinberg", "atkinson", "bayer8", "blue-noise"]) {
    for (const color_space of ["linear", "srgb"]) {
      cases.push({
        id: `space-${algorithm}-${color_space}`,
        recipe: { preprocess: { max_dim: 640 }, palette: pal("pico8"), dither: { algorithm, color_space } },
      });
    }
  }
  const prep = {
    "adaptive-8": { palette: { mode: "adaptive", value: 8 } },
    "adaptive-32": { palette: { mode: "adaptive", value: 32 }, dither: { algorithm: "atkinson" } },
    gamma: { preprocess: { gamma: 0.6 } },
    blur: { preprocess: { blur: 1.7 } },
    sharpen: { preprocess: { sharpen: 1.2 } },
    levels: { preprocess: { auto_levels: true, contrast: 0.3, saturation: 1.6, brightness: -0.1 } },
    "pixel-size": { preprocess: { pixel_size: 3 } },
    "resize-cover": { preprocess: { width: 517, height: 389, fit: "cover" } },
    "resize-scale": { preprocess: { scale: 1.37 } },
    grayscale: { preprocess: { grayscale: true, invert: true }, palette: pal("gray6") },
    "colors-noise": { palette: { mode: "colors", value: ["#1b1b3a", "#f2e9e4", "#e63946"].map(rgb) }, dither: { noise: 0.2, bias: 0.1, strength: 0.8 } },
    ink: { dither: { algorithm: "halftone", ink_below: 0.2, paper_above: 0.9, ink_lines: 0.5 } },
  };
  for (const [id, recipe] of Object.entries(prep)) cases.push({ id: `prep-${id}`, recipe });
  for (const { id } of w.generators()) {
    const generate = { pattern: id, width: 320, height: 240, phase: 0.37, stops: ["#0b1020", "#5b3cc4", "#f7d488"].map(rgb) };
    cases.push({ id: `gen-${id}`, generate });
    cases.push({ id: `gen-${id}-dithered`, generate, recipe: { palette: pal("pico8"), dither: { algorithm: "atkinson" } } });
  }
  return cases;
}

/** The benchmark matrix: four algorithms on 2- and 16-color palettes, plus a generator. */
export function benchCases() {
  const cases = [];
  for (const algorithm of ["floyd-steinberg", "atkinson", "bayer8", "blue-noise"]) {
    for (const palette of ["bw", "pico8"]) {
      cases.push({ id: `${algorithm}/${palette}`, recipe: { palette: pal(palette), dither: { algorithm } } });
    }
  }
  cases.push({
    id: "generate sphere 1920x1080 + atkinson/pico8",
    generate: { pattern: "sphere", width: 1920, height: 1080 },
    recipe: { palette: pal("pico8"), dither: { algorithm: "atkinson" } },
  });
  return cases;
}
