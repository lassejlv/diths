// A Cloudflare Worker that renders one parity case per request, through the package's
// `workerd` entry (the .wasm arrives as an imported WebAssembly.Module, no fetch).
import { dither, generate, init } from "../dist/workerd.js";

export default {
  async fetch(request) {
    const { input, generate: kind, options, format } = await request.json();
    const info = await init();
    const out = kind ? await generate(kind, options) : await dither(Uint8Array.from(atob(input), (c) => c.charCodeAt(0)), options);
    const bytes = format === "svg" ? new TextEncoder().encode(out.toSVG()) : format === "gif" ? out.toGIF() : out.toPNG();
    const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", bytes));
    return Response.json({ sha256: [...digest].map((b) => b.toString(16).padStart(2, "0")).join(""), decoders: info.decoders });
  },
};
