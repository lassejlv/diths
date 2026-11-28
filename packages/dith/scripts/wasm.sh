#!/usr/bin/env bash
# Build the two dith-wasm modules this package ships and the shared JS glue:
#   wasm/dith.wasm         default features (png, svg, gif output), for browsers
#   wasm/dith-decode.wasm  plus the `decode` feature (PNG, JPEG, GIF, WebP input), for servers
#   wasm/dith_wasm.js      wasm-bindgen's web glue, identical for both builds
# Needs what crates/dith-wasm/scripts/build.sh needs (wasm32 target, wasm-bindgen-cli, wasm-opt).
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
root="$(cd "$here/../.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

"$root/crates/dith-wasm/scripts/build.sh" --out "$tmp/lean"
"$root/crates/dith-wasm/scripts/build.sh" --features decode --out "$tmp/decode"

if ! cmp -s "$tmp/lean/web/dith_wasm.js" "$tmp/decode/web/dith_wasm.js"; then
  echo "the default and decode builds need different JS glue; keep their exports identical" >&2
  exit 1
fi

mkdir -p "$here/wasm"
cp "$tmp/lean/web/dith_wasm.js" "$tmp/lean/web/dith_wasm.d.ts" "$here/wasm/"
# The package always passes init a source, so drop the glue's own default URL: bundlers would
# otherwise look for a dith_wasm_bg.wasm that the package does not ship.
default_url="module_or_path = new URL('dith_wasm_bg.wasm', import.meta.url);"
grep -qF "$default_url" "$here/wasm/dith_wasm.js" || { echo "wasm-bindgen glue changed: update scripts/wasm.sh" >&2; exit 1; }
sed -i.bak "s|$default_url|throw new Error('dith: no wasm source');|" "$here/wasm/dith_wasm.js"
rm "$here/wasm/dith_wasm.js.bak"
cp "$tmp/lean/web/dith_wasm_bg.wasm" "$here/wasm/dith.wasm"
cp "$tmp/decode/web/dith_wasm_bg.wasm" "$here/wasm/dith-decode.wasm"
