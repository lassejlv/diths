#!/usr/bin/env node
// Write test/fixtures/fixtures.json: input images in every format the tests decode, and the
// SHA-256 of what the native dith CLI writes for each case. Every runtime's tests compare their
// output with these hashes, so passing tests mean byte-identical output to the CLI and to each
// other. Needs the dith binary ($DITH, else target/release/dith) and wasm/ from scripts/wasm.sh.
//
// The cases include the paths that call powf, exp, sin and cos (gamma, saturation, grayscale,
// blur, resizing, adaptive palettes, ostromoukhov and the curved generators): dith-core computes
// those with dith_core::detmath, so native and wasm agree to the bit there too.

import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import init, { decode, encode } from "../wasm/dith_wasm.js";

const here = resolve(import.meta.dirname, "..");
const root = resolve(here, "../..");
const dith = process.env.DITH ?? resolve(root, "target/release/dith");
const work = mkdtempSync(join(tmpdir(), "dith-fixtures-"));
const run = (...args) => execFileSync(dith, args, { cwd: work, stdio: ["ignore", "pipe", "inherit"] });

await init({ module_or_path: readFileSync(join(here, "wasm/dith-decode.wasm")) });

// A 400x300 crop of the intro poster, as a true-color PNG, plus a copy with an alpha ramp.
const jpeg = readFileSync(join(root, "assets/intro-poster.jpg"));
const poster = decode(jpeg);
const [cw, ch, x0, y0] = [400, 300, 440, 210];
const crop = new Uint8Array(cw * ch * 4);
for (let y = 0; y < ch; y++) crop.set(poster.rgba.subarray(((y0 + y) * poster.width + x0) * 4, ((y0 + y) * poster.width + x0 + cw) * 4), y * cw * 4);
const alpha = crop.slice();
for (let y = 0; y < ch; y++) for (let x = 0; x < cw; x++) alpha[(y * cw + x) * 4 + 3] = Math.round((x / (cw - 1)) * 255);

const inputs = { jpeg, png: Buffer.from(encode(crop, cw, ch, "png")), alpha: Buffer.from(encode(alpha, cw, ch, "png")) };
writeFileSync(join(work, "in.jpg"), inputs.jpeg);
writeFileSync(join(work, "in.png"), inputs.png);
writeFileSync(join(work, "alpha.png"), inputs.alpha);
run("apply", "in.png", "-o", "in.gif", "--preset", "pico8", "--json");
run("apply", "in.png", "-o", "in.webp", "--algorithm", "threshold", "--palette", "rgb332", "--json");
inputs.gif = readFileSync(join(work, "in.gif"));
inputs.webp = readFileSync(join(work, "in.webp"));
const files = { jpeg: "in.jpg", png: "in.png", alpha: "alpha.png", gif: "in.gif", webp: "in.webp" };

// [id, input or generator, library options, CLI flags, output format]
const cases = [
  ["jpeg-mac-classic", "jpeg", { preset: "mac-classic", maxDim: 480 }, ["--preset", "mac-classic", "--max-dim", "480"]],
  ["jpeg-manga", "jpeg", { preset: "manga", maxDim: 480 }, ["--preset", "manga", "--max-dim", "480"]],
  ["jpeg-risograph", "jpeg", { preset: "risograph", maxDim: 480 }, ["--preset", "risograph", "--max-dim", "480"]],
  ["png-film-stipple", "png", { preset: "film-stipple" }, ["--preset", "film-stipple"]],
  ["png-gameboy", "png", { preset: "gameboy", pixelSize: 2 }, ["--preset", "gameboy", "--pixel-size", "2"]],
  ["png-halftone-zine", "png", { preset: "halftone-zine" }, ["--preset", "halftone-zine"]],
  ["png-brand-colors", "png", { algorithm: "atkinson", colors: "#0f172a,#2563eb,#f5efe6", contrast: 0.2, sharpen: 0.5 }, ["--algorithm", "atkinson", "--colors", "#0f172a,#2563eb,#f5efe6", "--contrast", "0.2", "--sharpen", "0.5"]],
  ["png-pico8-cover", "png", { algorithm: "bayer8", palette: "pico8", width: 240, height: 240, fit: "cover", noise: 0.1, seed: 7 }, ["--algorithm", "bayer8", "--palette", "pico8", "--width", "240", "--height", "240", "--fit", "cover", "--noise", "0.1", "--seed", "7"]],
  ["png-svg-circles", "png", { preset: "halftone-zine", maxDim: 200, svgShape: "circle" }, ["--preset", "halftone-zine", "--max-dim", "200", "--svg-shape", "circle"], "svg"],
  ["png-gif", "png", { preset: "pico8" }, ["--preset", "pico8"], "gif"],
  ["gif-blueprint", "gif", { preset: "blueprint" }, ["--preset", "blueprint"]],
  ["webp-woodcut", "webp", { preset: "woodcut" }, ["--preset", "woodcut"]],
  ["alpha-dither", "alpha", { algorithm: "blue-noise", palette: "risograph-duo", alphaMode: "dither" }, ["--algorithm", "blue-noise", "--palette", "risograph-duo", "--alpha-mode", "dither"]],
  ["alpha-flatten", "alpha", { preset: "risograph-red", alphaMode: "flatten", background: "#f5efe6" }, ["--preset", "risograph-red", "--alpha-mode", "flatten", "--background", "#f5efe6"]],
  ["gen-sphere-risograph", { generate: "sphere" }, { preset: "risograph", canvasWidth: 320, canvasHeight: 240 }, ["--preset", "risograph", "--canvas-width", "320", "--canvas-height", "240"]],
  ["gen-rings-pico8", { generate: "rings" }, { algorithm: "bayer4", palette: "pico8", stops: ["#0b1020", "#5b3cc4", "#f7d488"], frequency: 1.5 }, ["--algorithm", "bayer4", "--palette", "pico8", "--stops", "#0b1020,#5b3cc4,#f7d488", "--frequency", "1.5"]],
  ["gen-linear-gradient", { generate: "linear-gradient" }, { preset: "film-stipple", width: 300, height: 120, angle: 30 }, ["--preset", "film-stipple", "--width", "300", "--height", "120", "--angle", "30"]],
  ["gen-noise-gameboy", { generate: "noise" }, { preset: "gameboy", seed: 9 }, ["--preset", "gameboy", "--seed", "9"]],
  ["png-gamma", "png", { algorithm: "floyd-steinberg", palette: "pico8", gamma: 0.6 }, ["--algorithm", "floyd-steinberg", "--palette", "pico8", "--gamma", "0.6"]],
  ["png-saturation", "png", { algorithm: "atkinson", palette: "pico8", saturation: 1.6 }, ["--algorithm", "atkinson", "--palette", "pico8", "--saturation", "1.6"]],
  ["png-grayscale-invert", "png", { grayscale: true, invert: true, palette: "gray8" }, ["--grayscale", "--invert", "--palette", "gray8"]],
  ["png-blur", "png", { palette: "gameboy", blur: 1.8 }, ["--palette", "gameboy", "--blur", "1.8"]],
  ["png-resize", "png", { algorithm: "floyd-steinberg", palette: "pico8", width: 333 }, ["--algorithm", "floyd-steinberg", "--palette", "pico8", "--width", "333"]],
  ["png-auto-palette", "png", { autoPalette: 8 }, ["--auto-palette", "8"]],
  ["png-ostromoukhov", "png", { algorithm: "ostromoukhov", palette: "pico8" }, ["--algorithm", "ostromoukhov", "--palette", "pico8"]],
  ["gen-radial-gradient", { generate: "radial-gradient" }, { algorithm: "floyd-steinberg", palette: "pico8", canvasWidth: 320, canvasHeight: 240 }, ["--algorithm", "floyd-steinberg", "--palette", "pico8", "--canvas-width", "320", "--canvas-height", "240"]],
  ["gen-mesh", { generate: "mesh" }, { algorithm: "floyd-steinberg", palette: "pico8", canvasWidth: 320, canvasHeight: 240 }, ["--algorithm", "floyd-steinberg", "--palette", "pico8", "--canvas-width", "320", "--canvas-height", "240"]],
  ["gen-plasma", { generate: "plasma" }, { algorithm: "floyd-steinberg", palette: "pico8", canvasWidth: 320, canvasHeight: 240 }, ["--algorithm", "floyd-steinberg", "--palette", "pico8", "--canvas-width", "320", "--canvas-height", "240"]],
  ["gen-waves", { generate: "waves" }, { preset: "risograph", canvasWidth: 320, canvasHeight: 240, angle: 30, frequency: 2.5 }, ["--preset", "risograph", "--canvas-width", "320", "--canvas-height", "240", "--angle", "30", "--frequency", "2.5"]],
  ["gen-checkerboard-svg", { generate: "checkerboard" }, { palette: "cga", algorithm: "threshold", canvasWidth: 64, canvasHeight: 48 }, ["--palette", "cga", "--algorithm", "threshold", "--canvas-width", "64", "--canvas-height", "48"], "svg"],
];

const out = [];
for (const [id, input, options, flags, format = "png"] of cases) {
  const file = `${id}.${format}`;
  const source = typeof input === "string" ? ["apply", files[input]] : ["generate", input.generate];
  run(...source, "-o", file, ...flags, "--json");
  const bytes = readFileSync(join(work, file));
  out.push({ id, ...(typeof input === "string" ? { input } : input), options, format, sha256: createHash("sha256").update(bytes).digest("hex"), bytes: bytes.length, cli: `dith ${[...source, "-o", file, ...flags].join(" ")}` });
}

const version = run("--version").toString().trim();
mkdirSync(join(here, "test/fixtures"), { recursive: true });
writeFileSync(
  join(here, "test/fixtures/fixtures.json"),
  JSON.stringify({ dith: version, inputs: Object.fromEntries(Object.entries(inputs).map(([k, v]) => [k, Buffer.from(v).toString("base64")])), cases: out }, null, 1),
);
rmSync(work, { recursive: true, force: true });
console.log(`wrote test/fixtures/fixtures.json: ${out.length} cases from ${version}`);
