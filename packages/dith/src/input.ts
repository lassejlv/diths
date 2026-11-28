import { invalid, toDithError } from "./errors.ts";
import { readURL, responseBytes, toURL } from "./io.ts";
import { type Glue, platformDecodes } from "./wasm.ts";
import type { Input, RgbaPixels } from "./types.ts";

const KINDS = [
  "ImageData or { data, width, height }",
  "Uint8Array / ArrayBuffer of PNG, JPEG, GIF or WebP bytes",
  "Blob / File",
  "Response",
  "URL or URL string (a file path on Node, Bun and Deno)",
  "HTMLImageElement, HTMLCanvasElement, HTMLVideoElement, ImageBitmap, OffscreenCanvas, VideoFrame",
];

const DRAWABLE = ["HTMLImageElement", "HTMLCanvasElement", "HTMLVideoElement", "ImageBitmap", "OffscreenCanvas", "VideoFrame"];

function isA(value: object, name: string): boolean {
  const ctor = (globalThis as Record<string, unknown>)[name];
  return typeof ctor === "function" && value instanceof ctor;
}

function isPixels(value: object): value is RgbaPixels {
  const v = value as Partial<RgbaPixels>;
  return (v.data instanceof Uint8ClampedArray || v.data instanceof Uint8Array) && typeof v.width === "number" && typeof v.height === "number";
}

type Drawable = CanvasImageSource & { width?: number; height?: number };

function canvas(width: number, height: number): OffscreenCanvasRenderingContext2D | CanvasRenderingContext2D {
  const surface =
    typeof OffscreenCanvas === "function"
      ? new OffscreenCanvas(width, height)
      : Object.assign(document.createElement("canvas"), { width, height });
  const ctx = surface.getContext("2d", { willReadFrequently: true }) as OffscreenCanvasRenderingContext2D | CanvasRenderingContext2D | null;
  if (!ctx) throw invalid("input", "this runtime has no 2D canvas to read pixels with");
  return ctx;
}

/** Pixels of a browser image source, drawn onto a canvas at its natural size. */
async function draw(source: Drawable): Promise<RgbaPixels> {
  if (isA(source, "HTMLImageElement")) await (source as HTMLImageElement).decode();
  const s = source as Partial<HTMLImageElement & HTMLVideoElement & VideoFrame>;
  const width = s.naturalWidth || s.videoWidth || s.displayWidth || s.width || 0;
  const height = s.naturalHeight || s.videoHeight || s.displayHeight || s.height || 0;
  if (!width || !height) throw invalid("input", "the image source has no pixels yet (0x0)", undefined, "Wait for the image or video to load before dithering it.");
  const ctx = canvas(width, height);
  ctx.drawImage(source, 0, 0, width, height);
  try {
    return ctx.getImageData(0, 0, width, height);
  } catch (err) {
    throw invalid("input", `cannot read the image's pixels: ${(err as Error).message}`, undefined, "Cross-origin images need CORS (crossorigin=\"anonymous\" and an Access-Control-Allow-Origin header).");
  }
}

/** Decode encoded image bytes: in WebAssembly when the module has decoders, else with the browser. */
async function decode(w: Glue, bytes: Uint8Array): Promise<RgbaPixels> {
  if (w.decoders().length) {
    try {
      const img = w.decode(bytes);
      return { data: img.rgba, width: img.width, height: img.height };
    } catch (err) {
      const unsupported = (err as { code?: string }).code === "INVALID_ARGUMENT";
      if (!unsupported || !platformDecodes()) throw toDithError(err);
    }
  }
  if (!platformDecodes()) throw invalid("input", "this module has no image decoders and the runtime has none either", undefined, "Load dith-decode.wasm with init({ decoders: true }), or pass RGBA pixels.");
  let bitmap: ImageBitmap;
  try {
    bitmap = await createImageBitmap(new Blob([bytes as Uint8Array<ArrayBuffer>]), { premultiplyAlpha: "none", colorSpaceConversion: "none" });
  } catch (err) {
    throw invalid("input", `the browser cannot decode these bytes: ${(err as Error).message}`, undefined, "Pass PNG, JPEG, GIF or WebP bytes, or RGBA pixels.");
  }
  try {
    return await draw(bitmap);
  } finally {
    bitmap.close();
  }
}

/** Turn any supported input into RGBA pixels. */
export async function toPixels(w: Glue, input: Input): Promise<RgbaPixels> {
  if (typeof input === "string" || input instanceof URL) return decode(w, await readURL(toURL(input)));
  if (typeof input !== "object" || input === null) throw invalid("input", `cannot read a ${input === null ? "null" : typeof input}`, KINDS);
  if (isPixels(input)) return input;
  if (input instanceof ArrayBuffer) return decode(w, new Uint8Array(input));
  if (ArrayBuffer.isView(input)) return decode(w, new Uint8Array(input.buffer, input.byteOffset, input.byteLength));
  if (typeof Blob === "function" && input instanceof Blob) return decode(w, new Uint8Array(await input.arrayBuffer()));
  if (typeof Response === "function" && input instanceof Response) return decode(w, await responseBytes(input));
  if (DRAWABLE.some((name) => isA(input, name))) return draw(input as Drawable);
  throw invalid("input", `cannot read a ${input.constructor?.name ?? "value"} of this shape`, KINDS);
}
