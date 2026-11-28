// Shared by every runtime's tests: the fixtures scripts/fixtures.mjs wrote from the native CLI.
import fixtures from "./fixtures/fixtures.json" with { type: "json" };

export interface Case {
  id: string;
  input?: keyof typeof fixtures.inputs;
  generate?: string;
  options: Record<string, unknown>;
  format: "png" | "svg" | "gif";
  sha256: string;
  bytes: number;
  cli: string;
}

export const cases = fixtures.cases as Case[];

export function input(name: keyof typeof fixtures.inputs): Uint8Array {
  return Uint8Array.from(atob(fixtures.inputs[name]), (c) => c.charCodeAt(0));
}

export async function sha256(bytes: Uint8Array | string): Promise<string> {
  const data = typeof bytes === "string" ? new TextEncoder().encode(bytes) : bytes;
  const digest = await crypto.subtle.digest("SHA-256", data as Uint8Array<ArrayBuffer>);
  return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

/** Whether a runtime decoding with the platform reproduces the case exactly: JPEG codecs and
 * canvas alpha premultiplication differ from the CLI, so those need the wasm decoders. */
export const platformExact = (c: Case): boolean => c.input !== "jpeg" && c.input !== "alpha";
