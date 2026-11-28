#!/usr/bin/env node
// No bundler: serve the package as static files and load dist/index.js from a plain
// <script type="module"> in headless Chromium. Checks the parity cases the browser decodes
// exactly and that only the lean dith.wasm is downloaded.
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { extname, resolve } from "node:path";
import { chromium } from "playwright";

const here = resolve(import.meta.dirname, "..");
const types = { ".html": "text/html", ".js": "text/javascript", ".json": "application/json", ".wasm": "application/wasm" };
const server = createServer(async (req, res) => {
  try {
    const body = await readFile(resolve(here, `.${new URL(req.url, "http://x").pathname}`));
    res.writeHead(200, { "content-type": types[extname(req.url)] ?? "application/octet-stream" }).end(body);
  } catch {
    res.writeHead(404).end();
  }
}).listen(0);
const port = server.address().port;

const browser = await chromium.launch();
const wasm = [];
let failed = 0;
try {
  const page = await browser.newPage();
  page.on("request", (r) => r.url().endsWith(".wasm") && wasm.push(new URL(r.url()).pathname));
  await page.goto(`http://localhost:${port}/test/cdn.html`);
  const result = await page.waitForFunction(() => window.__dith, null, { timeout: 60_000 }).then((h) => h.jsonValue());
  if (result.error) throw new Error(result.error);
  for (const r of result.results) {
    if (!r.ok) failed++;
    console.log(`${r.ok ? "ok  " : "FAIL"} ${r.id}`);
  }
  if (result.info.decoders.length || wasm.join() !== "/dist/dith.wasm") {
    failed++;
    console.log(`FAIL expected only /dist/dith.wasm, loaded ${wasm.join(", ")}`);
  }
  console.log(`no-bundler page: ${result.results.length - failed} passed, ${failed} failed; loaded ${wasm.join(", ")}`);
} finally {
  await browser.close();
  server.close();
}
process.exit(failed ? 1 : 0);
