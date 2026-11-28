import type { Recipe, ColorUsage, ImageDataLike, RgbaPixels } from "./types.ts";
import type { SvgShape } from "./generated.ts";
import { call, type Glue } from "./wasm.ts";

const MIME: Record<string, string> = { png: "image/png", gif: "image/gif", svg: "image/svg+xml", webp: "image/webp" };

/** Output formats of `encode()`, `toBlob()` and `toDataURL()`. WebP needs a dith-wasm build with the `webp` feature. */
export type Format = "png" | "gif" | "svg" | "webp";

/** `toSVG()` options; they default to the recipe's `svgShape` and `svgSkipBg`. */
export interface SvgOptions {
  svgShape?: SvgShape;
  svgSkipBg?: boolean;
}

interface Rendered {
  rgba: Uint8ClampedArray;
  width: number;
  height: number;
  palette?: string[];
  histogram?: ColorUsage[];
  source?: { width: number; height: number };
  working?: { width: number; height: number };
}

/**
 * A dithered image. It is `{ data, width, height }` pixels, so it can go straight back into
 * `dither()`, and it encodes itself to the same bytes the CLI writes.
 */
export class DitherResult implements RgbaPixels {
  /** RGBA pixels, 4 bytes per pixel, row by row. */
  readonly data: Uint8ClampedArray;
  readonly width: number;
  readonly height: number;
  /** The palette as hex colors, darkest-first as dith resolves it. */
  readonly palette: string[];
  /** How many pixels use each palette color. */
  readonly histogram: ColorUsage[];
  /** Size of the input before resizing. */
  readonly source: { width: number; height: number };
  /** Size the dither ran at (smaller than the output with `pixelSize`). */
  readonly working: { width: number; height: number };
  /**
   * The complete recipe this was rendered with. Pass it back as `{ recipe }`, or save it as JSON
   * for `dith apply --recipe`, to get the same pixels again.
   */
  readonly recipe: Recipe;
  readonly #w: Glue;

  /** @internal */
  constructor(w: Glue, out: Rendered, recipe: Recipe) {
    this.#w = w;
    this.data = out.rgba;
    this.width = out.width;
    this.height = out.height;
    this.palette = out.palette ?? [];
    this.histogram = out.histogram ?? [];
    this.source = out.source ?? { width: out.width, height: out.height };
    this.working = out.working ?? { width: out.width, height: out.height };
    this.recipe = recipe;
  }

  /** The pixels as `ImageData` where the runtime has it (browsers, Deno), else as `{ data, width, height }`. */
  toImageData(): ImageDataLike {
    if (typeof ImageData === "function") return new ImageData(new Uint8ClampedArray(this.data), this.width, this.height) as ImageDataLike;
    return { data: new Uint8ClampedArray(this.data), width: this.width, height: this.height } as ImageDataLike;
  }

  /** Encode as file bytes, exactly as `dith apply -o out.<format>` writes them. */
  encode(format: Format, options: SvgOptions = {}): Uint8Array {
    const exp = {
      ...this.recipe.export,
      ...(options.svgShape !== undefined && { svg_shape: options.svgShape }),
      ...(options.svgSkipBg !== undefined && { svg_skip_bg: options.svgSkipBg }),
    };
    return call(() => this.#w.encode(this.data, this.width, this.height, format, exp));
  }

  /** PNG bytes (indexed when the palette allows), identical to the CLI's. */
  toPNG(): Uint8Array {
    return this.encode("png");
  }

  /** GIF bytes. GIF holds at most 256 colors. */
  toGIF(): Uint8Array {
    return this.encode("gif");
  }

  /** An SVG document of run-length merged rects, circles or dots. */
  toSVG(options?: SvgOptions): string {
    return new TextDecoder().decode(this.encode("svg", options));
  }

  /** A `Blob` of the encoded image, for downloads, uploads and object URLs. */
  toBlob(format: Format = "png"): Blob {
    return new Blob([this.encode(format) as Uint8Array<ArrayBuffer>], { type: MIME[format] });
  }

  /** A `data:` URL of the encoded image, for `<img src>` and CSS. */
  toDataURL(format: Format = "png"): string {
    const bytes = this.encode(format);
    let binary = "";
    for (let i = 0; i < bytes.length; i += 0x8000) binary += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
    return `data:${MIME[format]};base64,${btoa(binary)}`;
  }
}
