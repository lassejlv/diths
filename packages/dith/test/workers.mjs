#!/usr/bin/env node
// Cloudflare Workers: run test/worker.js in workerd (through Miniflare) and check every parity case.
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { Miniflare } from "miniflare";

const here = resolve(import.meta.dirname, "..");
const { inputs, cases } = JSON.parse(readFileSync(resolve(here, "test/fixtures/fixtures.json"), "utf8"));
const mf = new Miniflare({
  modules: true,
  modulesRoot: here,
  scriptPath: resolve(here, "test/worker.js"),
  modulesRules: [
    { type: "ESModule", include: ["**/*.js"] },
    { type: "CompiledWasm", include: ["**/*.wasm"] },
  ],
  compatibilityDate: "2026-08-01",
});

let failed = 0;
try {
  for (const c of cases) {
    const res = await mf.dispatchFetch("http://worker/", {
      method: "POST",
      body: JSON.stringify({ input: c.input && inputs[c.input], generate: c.generate, options: c.options, format: c.format }),
    });
    const text = await res.text();
    const ok = res.ok && JSON.parse(text).sha256 === c.sha256;
    if (!ok) failed++;
    console.log(`${ok ? "ok  " : "FAIL"} ${c.id}${ok ? "" : ` ${text.slice(0, 300)}`}`);
  }
} finally {
  await mf.dispose();
}
console.log(`workerd: ${cases.length - failed} passed, ${failed} failed`);
process.exit(failed ? 1 : 0);
