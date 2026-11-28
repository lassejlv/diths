#!/usr/bin/env bash
# Build the dith-wasm npm package: the web, bundler and nodejs targets of wasm-bindgen, each
# shrunk with wasm-opt, plus package.json and README.md. Prints the .wasm size raw, gzip and brotli.
#
#   crates/dith-wasm/scripts/build.sh [--features "gif webp"] [--out DIR] [--opt-level 3|s|z]
#
# Needs the wasm32-unknown-unknown target, wasm-bindgen-cli at the version in Cargo.lock, and
# wasm-opt (binaryen) on PATH. --opt-level overrides [profile.wasm-release] for comparisons.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
crate="$root/crates/dith-wasm"
features=""
out="$crate/pkg"
opt_level=""
while [ $# -gt 0 ]; do
  case "$1" in
    --features) features="$2"; shift 2 ;;
    --out) out="$2"; shift 2 ;;
    --opt-level) opt_level="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

cargo_args=(build --manifest-path "$root/Cargo.toml" -p dith-wasm --target wasm32-unknown-unknown --profile wasm-release --locked)
[ -n "$features" ] && cargo_args+=(--features "$features")
[ -n "$opt_level" ] && cargo_args+=(--config "profile.wasm-release.opt-level=$( [ "$opt_level" = 3 ] && echo 3 || echo "\"$opt_level\"" )")
cargo "${cargo_args[@]}"

wasm="$root/target/wasm32-unknown-unknown/wasm-release/dith_wasm.wasm"
case "${opt_level:-3}" in
  3) wasm_opt_level=-O3 ;;
  s) wasm_opt_level=-Os ;;
  z) wasm_opt_level=-Oz ;;
esac

rm -rf "$out"
mkdir -p "$out"
for target in web bundler nodejs; do
  dir="$out/${target/nodejs/node}"
  wasm-bindgen --target "$target" --out-dir "$dir" "$wasm"
  wasm-opt "$wasm_opt_level" --strip-debug --strip-producers --strip-target-features \
    "$dir/dith_wasm_bg.wasm" -o "$dir/dith_wasm_bg.wasm"
  [ "$target" != nodejs ] && echo '{ "type": "module" }' > "$dir/package.json"
done

version="$(cargo metadata --manifest-path "$root/Cargo.toml" --format-version 1 --no-deps \
  | node -e 'const m = JSON.parse(require("fs").readFileSync(0, "utf8")); console.log(m.packages.find((p) => p.name === "dith-wasm").version)')"
sed "s/\"version\": \"0.0.0\"/\"version\": \"$version\"/" "$crate/package.json" > "$out/package.json"
cp "$crate/README.md" "$out/README.md"

bytes="$out/web/dith_wasm_bg.wasm"
raw=$(wc -c < "$bytes")
gz=$(gzip -9 -c "$bytes" | wc -c)
br="n/a"
command -v brotli > /dev/null && br=$(brotli -q 11 -c "$bytes" | wc -c)
echo "dith-wasm $version [${features:-default}] opt-level ${opt_level:-profile}: $raw bytes raw, $gz gzip -9, $br brotli -q 11"
