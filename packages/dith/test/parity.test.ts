import { describe, expect, test } from "vitest";
import { dither, generate, init, type Generator } from "../dist/index.js";
import { cases, input, platformExact, sha256 } from "./fixtures.ts";

const info = await init();
const wasmDecodes = info.decoders.length > 0;

describe(`same bytes as the dith CLI (${wasmDecodes ? "wasm" : "platform"} decoding)`, () => {
  for (const c of cases) {
    test.skipIf(!wasmDecodes && !platformExact(c))(c.id, async () => {
      const out = c.generate ? await generate(c.generate as Generator, c.options) : await dither(input(c.input!), c.options);
      const bytes = c.format === "svg" ? out.toSVG() : c.format === "gif" ? out.toGIF() : out.toPNG();
      expect(await sha256(bytes), c.cli).toBe(c.sha256);
    });
  }
});

test("replaying result.recipe reproduces the pixels", async () => {
  const first = await dither(input("png"), { preset: "manga", inkLines: 0.3 });
  const again = await dither(input("png"), { recipe: first.recipe });
  const fromJson = await dither(input("png"), { recipe: JSON.stringify(first.recipe) });
  expect(await sha256(again.toPNG())).toBe(await sha256(first.toPNG()));
  expect(await sha256(fromJson.toPNG())).toBe(await sha256(first.toPNG()));
});
