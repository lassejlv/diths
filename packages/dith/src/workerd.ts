// Cloudflare Workers entry (the `workerd` export condition): Workers cannot fetch the .wasm next
// to the script, so it is imported as a module, which wrangler bundles as CompiledWasm.
import wasm from "./dith-decode.wasm";
import { setDefaultWasm } from "./wasm.ts";

setDefaultWasm(wasm);

export * from "./index.ts";
