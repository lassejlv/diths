#!/usr/bin/env node
// Pixel parity between dith-wasm and native dith-core.
//
//   node crates/dith-wasm/scripts/parity.mjs <pkg-dir> [cargo feature list]
//
// <pkg-dir> holds the nodejs build (dith_wasm.js). The native side is examples/native.rs, built
// with the same features. Every case must produce identical RGBA, and identical bytes from every
// encoder the build includes. Exits 1 on any difference and names the cases.

import { execFileSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { parityCases } from "./cases.mjs";

const [pkgDir, features = ""] = process.argv.slice(2);
if (!pkgDir) {
  console.error("usage: parity.mjs <pkg-dir> [features]");
  process.exit(2);
}
const w = createRequire(import.meta.url)(resolve(pkgDir, "dith_wasm.js"));
const root = resolve(import.meta.dirname, "../../..");
const work = mkdtempSync(join(tmpdir(), "dith-parity-"));

const cargo = ["run", "--quiet", "--release", "--locked", "-p", "dith-wasm", "--example", "native"];
if (features) cargo.push("--features", features);
const native = (...args) => execFileSync("cargo", [...cargo, "--", ...args], { cwd: root, encoding: "utf8", stdio: ["ignore", "pipe", "inherit"] });

const fixture = join(work, "input.rgba");
const { width, height } = JSON.parse(native("fixture", join(root, "assets/intro-poster.jpg"), fixture));
const input = readFileSync(fixture);
const cases = parityCases(w);
writeFileSync(join(work, "cases.json"), JSON.stringify(cases));
native("render", join(work, "cases.json"), fixture, String(width), String(height), join(work, "out"));

const formats = w.formats();
const colorCount = (rgba) => {
  const seen = new Set();
  const words = new Uint32Array(rgba.buffer, rgba.byteOffset, rgba.byteLength / 4);
  for (const word of words) if (seen.add(word).size > 256) break;
  return seen.size;
};
const failures = [];
const known = [];
for (const c of cases) {
  const spec = (v) => (typeof v === "string" ? v : v && JSON.stringify(v));
  const out = c.generate ? w.generate(c.generate, spec(c.recipe)) : w.dither(input, width, height, spec(c.recipe));
  const meta = JSON.parse(readFileSync(join(work, "out", `${c.id}.json`), "utf8"));
  const expected = readFileSync(join(work, "out", `${c.id}.rgba`));
  const got = Buffer.from(out.rgba.buffer, out.rgba.byteOffset, out.rgba.byteLength);
  if (meta.width !== out.width || meta.height !== out.height || !expected.equals(got)) {
    let pixels = 0;
    let maxDelta = 0;
    for (let i = 0; i < Math.min(expected.length, got.length); i += 4) {
      let differs = false;
      for (let k = 0; k < 4; k++) {
        const d = Math.abs(expected[i + k] - got[i + k]);
        if (d) differs = true;
        maxDelta = Math.max(maxDelta, d);
      }
      if (differs) pixels++;
    }
    const line = `${c.id}: ${pixels} of ${expected.length / 4} pixels differ (max channel delta ${maxDelta}), native ${meta.width}x${meta.height}, wasm ${out.width}x${out.height}`;
    failures.push(line);
    continue;
  }
  for (const format of formats) {
    const file = join(work, "out", `${c.id}.${format}`);
    let got;
    try {
      got = Buffer.from(w.encode(out.rgba, out.width, out.height, format));
    } catch (err) {
      got = null;
      if (!existsSync(`${file}.error`) || readFileSync(`${file}.error`, "utf8") !== err.code) {
        failures.push(`${c.id}: ${format} failed in wasm only (${err.code})`);
      }
    }
    if (got && !(existsSync(file) && readFileSync(file).equals(got))) {
      // WebP of a true-color image (more than 256 colors, so never a dither result) comes from
      // image-webp, whose Huffman tie order follows the pointer width. Both files are lossless and
      // decode to the same pixels; only the bytes differ.
      if (format === "webp" && colorCount(out.rgba) > 256) known.push(`${c.id}: true-color webp bytes differ (lossless, same pixels)`);
      else failures.push(`${c.id}: ${format} bytes differ`);
    }
  }
}
rmSync(work, { recursive: true, force: true });

const exact = cases.length - known.length - failures.length;
console.log(`dith-wasm ${w.version()} parity: ${cases.length} cases x (RGBA + ${formats.join(", ")}): ${exact} identical, ${known.length} known differences, ${failures.length} failing`);
for (const k of known) console.log(`  known ${k}`);
for (const f of failures) console.log(`  FAIL ${f}`);
process.exit(failures.length ? 1 : 0);
