import { invalid } from "./errors.ts";

type Fs = { readFile(path: URL): Promise<Uint8Array> };
type Runtime = {
  process?: { cwd?(): string; getBuiltinModule?(id: string): unknown };
  Deno?: { cwd(): string };
  location?: { href?: string };
};
const runtime = globalThis as Runtime;

/** `node:fs/promises` where `process.getBuiltinModule` exists (Node >= 20.16, Bun, Deno); no
 * import statement, so bundlers and browsers never see it. Elsewhere `file:` URLs go to `fetch`. */
function fs(): Fs | undefined {
  return runtime.process?.getBuiltinModule?.("node:fs/promises") as Fs | undefined;
}

function pageHref(): string | undefined {
  try {
    return runtime.location?.href;
  } catch {
    return undefined;
  }
}

/**
 * A URL string as a `URL`. Relative strings resolve against the page in browsers and against the
 * working directory on servers, where they are file paths.
 */
export function toURL(src: string | URL): URL {
  if (src instanceof URL) return src;
  if (/^[a-z][a-z\d+.-]+:/i.test(src)) return new URL(src);
  const href = pageHref();
  if (href) return new URL(src, href);
  const node = runtime.process?.getBuiltinModule?.("node:url") as { pathToFileURL(p: string): URL } | undefined;
  if (node) return node.pathToFileURL(src);
  const cwd = (runtime.process?.cwd?.() ?? runtime.Deno?.cwd() ?? "/").replace(/\\/g, "/").replace(/\/?$/, "/");
  const path = src.replace(/\\/g, "/");
  return new URL(path.startsWith("/") || /^[a-z]:\//i.test(path) ? `file:///${path.replace(/^\//, "")}` : `file://${cwd.startsWith("/") ? "" : "/"}${cwd}${path}`);
}

/** The body of a fetch response, failing on HTTP errors. */
export async function responseBytes(res: Response): Promise<Uint8Array> {
  if (!res.ok) {
    throw invalid("input", `fetching ${res.url || "the input"} returned HTTP ${res.status}`, undefined, "Check the URL is reachable and serves an image (and allows CORS in browsers).");
  }
  return new Uint8Array(await res.arrayBuffer());
}

/** Read a URL: the file system for `file:` URLs where there is one, `fetch` otherwise. */
export async function readURL(url: URL): Promise<Uint8Array> {
  if (url.protocol === "file:") {
    const files = fs();
    if (files) {
      try {
        return new Uint8Array(await files.readFile(url));
      } catch (err) {
        throw invalid("input", `cannot read ${url.pathname}: ${(err as Error).message}`, undefined, "Check the path exists; relative paths resolve against the working directory.");
      }
    }
  }
  let res: Response;
  try {
    res = await fetch(url);
  } catch (err) {
    throw invalid("input", `cannot fetch ${url.href}: ${(err as Error).message}`, undefined, "Check the URL is reachable and serves an image (and allows CORS in browsers).");
  }
  return responseBytes(res);
}
