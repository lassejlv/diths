#!/usr/bin/env node
// Milliseconds per image for dith-wasm in Node against native dith-core on the same cases.
//
//   node crates/dith-wasm/scripts/bench.mjs <pkg-dir> <photo> [runs]
//
// Native runs examples/native.rs twice: on one thread (RAYON_NUM_THREADS=1, the same work the
// single-threaded wasm does) and on every core. Each figure is the median of [runs] (default 7)
// after one warm-up, and includes copying pixels into and out of wasm memory.

import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { benchCases } from "./cases.mjs";

const [pkgDir, photo, runsArg = "7"] = process.argv.slice(2);
if (!pkgDir || !photo) {
  console.error("usage: bench.mjs <pkg-dir> <photo> [runs]");
  process.exit(2);
}
const runs = Number(runsArg);
const w = createRequire(import.meta.url)(resolve(pkgDir, "dith_wasm.js"));
const root = resolve(import.meta.dirname, "../../..");
const work = mkdtempSync(join(tmpdir(), "dith-bench-"));
const cargo = ["run", "--quiet", "--release", "--locked", "-p", "dith-wasm", "--example", "native", "--"];
const native = (args, env = {}) =>
  execFileSync("cargo", [...cargo, ...args], { cwd: root, encoding: "utf8", env: { ...process.env, ...env }, stdio: ["ignore", "pipe", "inherit"] });

const fixture = join(work, "input.rgba");
const { width, height } = JSON.parse(native(["fixture", resolve(photo), fixture]));
const input = new Uint8Array(readFileSync(fixture));
const cases = benchCases();
writeFileSync(join(work, "cases.json"), JSON.stringify(cases));
const nativeArgs = ["bench", join(work, "cases.json"), fixture, String(width), String(height), String(runs)];
const one = JSON.parse(native(nativeArgs, { RAYON_NUM_THREADS: "1" }));
const all = JSON.parse(native(nativeArgs));
rmSync(work, { recursive: true, force: true });

const spec = (v) => (typeof v === "string" ? v : v && JSON.stringify(v));
const run = (c) => (c.generate ? w.generate(c.generate, spec(c.recipe)) : w.dither(input, width, height, spec(c.recipe)));
const rows = [];
for (const c of cases) {
  run(c);
  const times = [];
  for (let i = 0; i < runs; i++) {
    const t = performance.now();
    run(c);
    times.push(performance.now() - t);
  }
  times.sort((a, b) => a - b);
  const wasm = times[times.length >> 1];
  rows.push({ case: c.id, wasm, one: one[c.id], all: all[c.id] });
}
const f = (ms) => ms.toFixed(1).padStart(8);
console.log(`${width}x${height}, median of ${runs} runs, ms per image\n`);
console.log("| case | wasm (Node) | native 1 thread | native all cores | wasm / native 1T |");
console.log("|---|---:|---:|---:|---:|");
for (const r of rows) console.log(`| ${r.case} | ${f(r.wasm)} | ${f(r.one)} | ${f(r.all)} | ${(r.wasm / r.one).toFixed(2)}x |`);
