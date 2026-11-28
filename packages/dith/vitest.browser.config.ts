import { playwright } from "@vitest/browser-playwright";
import { defineConfig } from "vitest/config";

// Headless Chromium through Vite, so this also checks that a bundler finds the package's .wasm.
export default defineConfig({
  test: {
    include: ["test/*.test.ts", "test/browser/*.ts"],
    browser: {
      enabled: true,
      headless: true,
      provider: playwright(),
      instances: [{ browser: "chromium" }],
    },
  },
});
