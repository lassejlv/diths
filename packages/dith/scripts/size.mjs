#!/usr/bin/env node
// Size report: the JS a browser downloads (dist/index.js and its chunk) as shipped and minified,
// gzip and brotli, then both .wasm modules. Run after `bun run build`.
import { mkdtempSync, readFileSync, readdirSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { brotliCompressSync, constants, gzipSync } from "node:zlib";
import { build } from "tsdown";

const here = resolve(import.meta.dirname, "..");
const kb = (n) => `${(n / 1000).toLocaleString("en-US", { minimumFractionDigits: 1, maximumFractionDigits: 1 })} KB`;
const gz = (b) => gzipSync(b, { level: 9 }).length;
const br = (b) => brotliCompressSync(b, { params: { [constants.BROTLI_PARAM_QUALITY]: 11 } }).length;
const js = (dir) => Buffer.concat(readdirSync(dir).filter((f) => /^(index|core-.*)\.js$/.test(f)).map((f) => readFileSync(join(dir, f))));

const min = mkdtempSync(join(tmpdir(), "dith-size-"));
await build({ config: resolve(here, "tsdown.config.ts"), outDir: min, minify: true, dts: false, copy: [], logLevel: "silent" });
const shipped = js(resolve(here, "dist"));
const minified = js(min);
rmSync(min, { recursive: true, force: true });

const rows = [
  ["JS (dist/index.js + chunk), as shipped", shipped],
  ["JS, minified", minified],
  ["dith.wasm (browsers)", readFileSync(resolve(here, "dist/dith.wasm"))],
  ["dith-decode.wasm (Node, Bun, Deno, Workers)", readFileSync(resolve(here, "dist/dith-decode.wasm"))],
];
console.log("| File | Raw | gzip -9 | brotli -q 11 |\n| --- | ---: | ---: | ---: |");
for (const [name, bytes] of rows) console.log(`| ${name} | ${kb(bytes.length)} | ${kb(gz(bytes))} | ${kb(br(bytes))} |`);
