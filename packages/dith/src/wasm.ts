import load, {
  algorithms,
  decode,
  decoders,
  dither,
  encode,
  formats,
  generate,
  generators,
  type InitInput,
  palettes,
  preset,
  presets,
  recipe,
  version,
} from "../wasm/dith_wasm.js";
import { DithError, toDithError } from "./errors.ts";
import { readURL, toURL } from "./io.ts";
import type { DithInfo, InitOptions, WasmSource } from "./types.ts";

/** The wasm-bindgen exports of dith-wasm. */
export type Glue = Omit<typeof import("../wasm/dith_wasm.js"), "default" | "initSync">;

const glue: Glue = { algorithms, decode, decoders, dither, encode, formats, generate, generators, palettes, preset, presets, recipe, version };

let loading: Promise<DithInfo> | undefined;
let implicit = false;
let bundled: WasmSource | undefined;

/** Set the module `init()` loads by default (the Workers entry passes its imported .wasm). */
export function setDefaultWasm(source: WasmSource): void {
  bundled = source;
}

/** Whether this runtime decodes images itself: browsers and web workers, not Deno (whose
 * OffscreenCanvas has no 2D context). */
export function platformDecodes(): boolean {
  const g = globalThis as { document?: unknown; WorkerGlobalScope?: unknown; Deno?: unknown };
  return typeof createImageBitmap === "function" && (g.document !== undefined || g.WorkerGlobalScope !== undefined) && g.Deno === undefined;
}

function isOptions(value: unknown): value is InitOptions {
  return typeof value === "object" && value !== null && Object.getPrototypeOf(value) === Object.prototype;
}

async function instantiate({ wasm, decoders }: InitOptions): Promise<DithInfo> {
  let source = wasm ?? bundled;
  if (source === undefined) {
    source = decoders || !platformDecodes() ? new URL("./dith-decode.wasm", import.meta.url) : new URL("./dith.wasm", import.meta.url);
  }
  if (typeof source === "string") source = toURL(source);
  if (source instanceof URL && source.protocol === "file:") source = await readURL(source);
  await load({ module_or_path: source as InitInput | Promise<InitInput> });
  return { version: glue.version(), formats: glue.formats(), decoders: glue.decoders() };
}

/**
 * Load the WebAssembly module. Every call does this on first use, so `init()` is only for
 * controlling when it happens or where the .wasm comes from:
 *
 * ```ts
 * await init();                                  // the package's own .wasm, now
 * await init({ wasm: "/assets/dith.wasm" });     // a URL you host
 * await init({ wasm: bytes });                   // ArrayBuffer, Uint8Array, Response or WebAssembly.Module
 * await init({ decoders: true });                // CLI-identical image decoding in browsers
 * ```
 *
 * Resolves to what the module can do. Call it before anything else to pick the source; the first
 * load wins.
 */
export function init(options?: InitOptions | WasmSource): Promise<DithInfo> {
  const opts = options === undefined ? {} : isOptions(options) ? options : { wasm: options };
  const explicit = opts.wasm !== undefined || opts.decoders !== undefined;
  if (loading && explicit && implicit) {
    return Promise.reject(
      new DithError(
        "INVALID_ARGUMENT",
        "invalid argument 'init': dith already loaded its default module, so these init() options came too late",
        "Call init(options) before the first dither(), generate() or catalog call.",
      ),
    );
  }
  if (!loading) {
    implicit = !explicit;
    loading = instantiate(opts).catch((err: unknown) => {
      loading = undefined;
      throw toDithError(err);
    });
  }
  return loading;
}

/** The loaded module, loading the default one on first use. */
export async function ready(): Promise<Glue> {
  await init();
  return glue;
}

/** Run a module call, rethrowing its errors as `DithError`. */
export function call<T>(fn: () => T): T {
  try {
    return fn();
  } catch (err) {
    throw toDithError(err);
  }
}
