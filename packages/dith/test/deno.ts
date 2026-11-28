// Deno: the parity cases plus file-path input, run with `deno run --allow-read --allow-env test/deno.ts`.
import { dither, generate, init, type Generator } from "../dist/index.js";
import { cases, input, sha256 } from "./fixtures.ts";

const info = await init();
let failed = 0;
for (const c of cases) {
  const out = c.generate ? await generate(c.generate as Generator, c.options) : await dither(input(c.input!), c.options);
  const bytes = c.format === "svg" ? out.toSVG() : c.format === "gif" ? out.toGIF() : out.toPNG();
  const ok = (await sha256(bytes)) === c.sha256;
  if (!ok) failed++;
  console.log(`${ok ? "ok  " : "FAIL"} ${c.id}`);
}
const fromPath = await dither(new URL("../../../assets/intro-poster.jpg", import.meta.url).pathname, { preset: "manga", maxDim: 480 });
const ok = (await sha256(fromPath.toPNG())) === cases.find((c) => c.id === "jpeg-manga")!.sha256;
if (!ok) failed++;
console.log(`${ok ? "ok  " : "FAIL"} file path input`);
console.log(`deno ${Deno.version.deno}, dith-wasm ${info.version} (decoders: ${info.decoders.join(", ")}): ${cases.length + 1 - failed} passed, ${failed} failed`);
if (failed) Deno.exit(1);
