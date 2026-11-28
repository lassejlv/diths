// The parity cases again in the browser with dith-decode.wasm, which decodes JPEG and
// translucent PNG exactly as the CLI does where the browser's own codecs cannot.
import { expect, test } from "vitest";
import { init } from "../../dist/index.js";

const info = await init({ decoders: true });

test("init({ decoders: true }) loads the decoders", () => {
  expect(info.decoders).toEqual(["png", "jpeg", "gif", "webp"]);
});

await import("../parity.test.ts");
