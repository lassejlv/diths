import { describe, expect, test } from "vitest";
import { algorithms, DithError, DitherResult, dither, generate, generators, init, palettes, presets, recipe } from "../dist/index.js";
import { input, sha256 } from "./fixtures.ts";

const info = await init();
const browser = typeof document !== "undefined";
const png = input("png");
const expected = await sha256((await dither(png, "gameboy")).toPNG());
const hash = async (out: DitherResult) => sha256(out.toPNG());

describe("inputs", () => {
  test("Uint8Array of PNG bytes", async () => {
    expect(await hash(await dither(png, "gameboy"))).toBe(expected);
  });
  test("ArrayBuffer", async () => {
    expect(await hash(await dither(png.slice().buffer, "gameboy"))).toBe(expected);
  });
  test("a view into a larger buffer", async () => {
    const padded = new Uint8Array(png.length + 16);
    padded.set(png, 8);
    expect(await hash(await dither(new DataView(padded.buffer, 8, png.length), "gameboy"))).toBe(expected);
  });
  test("Blob and File", async () => {
    expect(await hash(await dither(new Blob([png as Uint8Array<ArrayBuffer>], { type: "image/png" }), "gameboy"))).toBe(expected);
    expect(await hash(await dither(new File([png as Uint8Array<ArrayBuffer>], "in.png"), "gameboy"))).toBe(expected);
  });
  test("Response", async () => {
    expect(await hash(await dither(new Response(png as Uint8Array<ArrayBuffer>), "gameboy"))).toBe(expected);
  });
  test("data: URL string and URL object", async () => {
    let binary = "";
    for (const b of png) binary += String.fromCharCode(b);
    const url = `data:image/png;base64,${btoa(binary)}`;
    expect(await hash(await dither(url, "gameboy"))).toBe(expected);
    expect(await hash(await dither(new URL(url), "gameboy"))).toBe(expected);
  });
  test("{ data, width, height } pixels and a previous result", async () => {
    const first = await dither(png, "gameboy");
    const pixels = { data: new Uint8Array(first.data), width: first.width, height: first.height };
    const viaPixels = await dither(pixels, { algorithm: "threshold", palette: "gameboy" });
    const viaResult = await dither(first, { algorithm: "threshold", palette: "gameboy" });
    expect(viaPixels.width).toBe(first.width);
    expect(await hash(viaPixels)).toBe(await hash(viaResult));
  });
  test.skipIf(browser || typeof process === "undefined")("file path and file: URL", async () => {
    const { mkdtempSync, writeFileSync } = await import("node:fs");
    const { tmpdir } = await import("node:os");
    const { join, relative } = await import("node:path");
    const { pathToFileURL } = await import("node:url");
    const file = join(mkdtempSync(join(tmpdir(), "dith-")), "in.png");
    writeFileSync(file, png);
    expect(await hash(await dither(file, "gameboy"))).toBe(expected);
    expect(await hash(await dither(relative(process.cwd(), file), "gameboy"))).toBe(expected);
    expect(await hash(await dither(pathToFileURL(file), "gameboy"))).toBe(expected);
  });
  test.skipIf(!browser)("ImageData, canvases, ImageBitmap, <img> and blob: URLs", async () => {
    const blob = new Blob([png as Uint8Array<ArrayBuffer>], { type: "image/png" });
    const bitmap = await createImageBitmap(blob, { premultiplyAlpha: "none", colorSpaceConversion: "none" });
    const offscreen = new OffscreenCanvas(bitmap.width, bitmap.height);
    offscreen.getContext("2d")!.drawImage(bitmap, 0, 0);
    const canvas = Object.assign(document.createElement("canvas"), { width: bitmap.width, height: bitmap.height });
    canvas.getContext("2d")!.drawImage(bitmap, 0, 0);
    const imageData = canvas.getContext("2d")!.getImageData(0, 0, canvas.width, canvas.height);
    const url = URL.createObjectURL(blob);
    const img = Object.assign(new Image(), { src: url });
    for (const source of [imageData, bitmap, offscreen, canvas, img, url]) {
      expect(await hash(await dither(source, "gameboy")), String(source)).toBe(expected);
    }
    URL.revokeObjectURL(url);
  });
  test("every encoded format decodes", async () => {
    for (const name of ["png", "gif", "webp", ...(info.decoders.length ? ["jpeg"] : [])] as const) {
      const out = await dither(input(name), "pico8");
      expect(out.source).toEqual(name === "jpeg" ? { width: 1280, height: 720 } : { width: 400, height: 300 });
    }
  });
});

describe("outputs", () => {
  test("result fields", async () => {
    const out = await dither(png, { preset: "risograph-duo", maxDim: 200 });
    expect(out).toBeInstanceOf(DitherResult);
    expect([out.width, out.height]).toEqual([200, 150]);
    expect(out.data.length).toBe(200 * 150 * 4);
    expect(out.source).toEqual({ width: 400, height: 300 });
    expect(out.palette.length).toBeGreaterThanOrEqual(2);
    expect(out.histogram.reduce((n, c) => n + c.pixels, 0)).toBe(200 * 150);
    expect(out.recipe.preset).toBe("risograph-duo");
    expect(out.recipe.preprocess?.max_dim).toBe(200);
  });
  test("toImageData", async () => {
    const out = await dither(png, "mac-classic");
    const data = out.toImageData();
    expect([data.width, data.height]).toEqual([out.width, out.height]);
    expect(data.data).toEqual(out.data);
    if (typeof ImageData === "function") expect(data).toBeInstanceOf(ImageData);
  });
  test("toPNG, toGIF, toSVG, encode", async () => {
    const out = await dither(png, { preset: "pico8", maxDim: 64 });
    expect([...out.toPNG().subarray(1, 4)].map((c) => String.fromCharCode(c)).join("")).toBe("PNG");
    expect(new TextDecoder().decode(out.toGIF().subarray(0, 6))).toBe("GIF89a");
    expect(out.toSVG()).toMatch(/^<svg /);
    expect(out.toSVG({ svgShape: "circle" })).toMatch(/a\.48\.48 /);
    expect(out.toSVG({ svgShape: "circle" })).not.toBe(out.toSVG());
    expect(out.encode("png")).toEqual(out.toPNG());
    expect(info.formats).toEqual(expect.arrayContaining(["png", "svg", "gif"]));
  });
  test("toBlob and toDataURL", async () => {
    const out = await dither(png, { preset: "pico8", maxDim: 64 });
    const blob = out.toBlob();
    expect(blob.type).toBe("image/png");
    expect(new Uint8Array(await blob.arrayBuffer())).toEqual(out.toPNG());
    expect(out.toBlob("svg").type).toBe("image/svg+xml");
    const url = out.toDataURL("gif");
    expect(url.startsWith("data:image/gif;base64,")).toBe(true);
    expect(Uint8Array.from(atob(url.split(",")[1]), (c) => c.charCodeAt(0))).toEqual(out.toGIF());
  });
});

describe("generate and catalogs", () => {
  test("generate draws the canvas size and honours width/height like the CLI", async () => {
    const a = await generate("sphere", { preset: "risograph", canvasWidth: 120, canvasHeight: 80 });
    expect([a.width, a.height]).toEqual([120, 80]);
    const b = await generate("sphere", { preset: "risograph", width: 120, height: 80, canvasWidth: 999 });
    expect(await hash(b)).toBe(await hash(a));
  });
  test("catalogs", async () => {
    expect((await presets()).map((p) => p.id)).toContain("manga");
    expect((await palettes()).find((p) => p.id === "bw")?.colors).toEqual(["#000000", "#ffffff"]);
    expect((await algorithms()).map((a) => a.id)).toContain("atkinson");
    expect((await generators()).map((g) => g.id)).toContain("sphere");
  });
  test("recipe() resolves options like --emit-recipe", async () => {
    const r = await recipe({ preset: "gameboy", pixelSize: 3, colors: ["#000", "#fff"], background: "#f5efe6" });
    expect(r.preset).toBe("gameboy");
    expect(r.preprocess?.pixel_size).toBe(3);
    expect(r.palette).toEqual({ mode: "colors", value: [{ r: 0, g: 0, b: 0 }, { r: 255, g: 255, b: 255 }] });
    expect(r.preprocess?.background).toEqual({ r: 245, g: 239, b: 230 });
  });
});

describe("errors", () => {
  const fails = async (promise: Promise<unknown>) => {
    try {
      await promise;
    } catch (err) {
      expect(err).toBeInstanceOf(DithError);
      return err as DithError;
    }
    throw new Error("expected a DithError");
  };
  test("UNKNOWN_PRESET with validValues", async () => {
    const err = await fails(dither(png, "nope" as never));
    expect(err.code).toBe("UNKNOWN_PRESET");
    expect(err.validValues).toContain("manga");
    expect(err.suggestion).toMatch(/presets/);
    expect(err.toJSON()).toMatchObject({ status: "error", code: "UNKNOWN_PRESET", valid_values: err.validValues });
  });
  test("UNKNOWN_PALETTE", async () => {
    const err = await fails(dither(png, { palette: "nope" as never }));
    expect(err.code).toBe("UNKNOWN_PALETTE");
    expect(err.validValues).toContain("pico8");
  });
  test("INVALID_ARGUMENT for ranges, choices, unknown options and inputs", async () => {
    expect((await fails(dither(png, { strength: 9 }))).message).toMatch(/strength.*0\.0\.\.=1\.5/);
    const choice = await fails(dither(png, { algorithm: "nope" as never }));
    expect(choice.validValues).toContain("atkinson");
    const typo = await fails(dither(png, { pallete: "bw" } as never));
    expect([typo.code, typo.validValues?.includes("palette")]).toEqual(["INVALID_ARGUMENT", true]);
    expect((await fails(dither(new Uint8Array([1, 2, 3])))).code).toBe("INVALID_ARGUMENT");
    expect((await fails(dither({ data: new Uint8Array(7), width: 2, height: 2 }))).code).toBe("INVALID_ARGUMENT");
    expect((await fails(dither(42 as never))).validValues?.length).toBeGreaterThan(3);
    expect((await fails(generate("nope" as never))).validValues).toContain("sphere");
  });
  test("INVALID_HEX_COLOR and INVALID_RECIPE_JSON", async () => {
    expect((await fails(dither(png, { colors: "#12,#fff" }))).code).toBe("INVALID_HEX_COLOR");
    expect((await fails(dither(png, { recipe: "{ nope" }))).code).toBe("INVALID_RECIPE_JSON");
  });
  test("init options after the first load are refused", async () => {
    expect((await fails(init({ decoders: true }))).code).toBe("INVALID_ARGUMENT");
  });
});
