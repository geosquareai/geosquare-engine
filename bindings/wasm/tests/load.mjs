// Shared setup for the test files: loads the wasm module once and reads spec 2.0 vectors.
import { readFileSync } from "node:fs";

export const gs = await import("../pkg/geosquare_wasm.js");
await gs.default({
  module_or_path: readFileSync(new URL("../pkg/geosquare_wasm_bg.wasm", import.meta.url)),
});

const VECTORS = new URL("../../../vectors/spec-2.0/", import.meta.url);

export function vector(name) {
  return JSON.parse(readFileSync(new URL(name, VECTORS), "utf8"));
}
