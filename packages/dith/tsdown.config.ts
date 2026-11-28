import { defineConfig } from "tsdown";

export default defineConfig({
  entry: ["src/index.ts", "src/workerd.ts"],
  format: "esm",
  platform: "neutral",
  target: "es2022",
  dts: { generator: "oxc" },
  deps: { neverBundle: [/\.wasm$/] },
  outputOptions: { chunkFileNames: "core-[hash].js" },
  copy: [
    { from: "wasm/dith.wasm", to: "dist" },
    { from: "wasm/dith-decode.wasm", to: "dist" },
  ],
});
