#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

echo "Building on-this-day-sidecar sidecar..."
cargo build --manifest-path "$SCRIPT_DIR/sidecar/Cargo.toml" --target wasm32-wasip1 --release
cp "$SCRIPT_DIR/../target/wasm32-wasip1/release/on-this-day-sidecar.wasm" "$SCRIPT_DIR/sidecar.wasm"
SIDECAR_SIZE=$(wc -c < "$SCRIPT_DIR/sidecar.wasm")
echo "Done: sidecar.wasm (${SIDECAR_SIZE} bytes)"

echo "Building on_this_day_slide.wasm..."
cargo build --target wasm32-wasip1 --release
cp "../target/wasm32-wasip1/release/on_this_day_slide.wasm" on_this_day_slide.wasm
ln -sfn on_this_day_slide.wasm slide.wasm
ln -sfn on_this_day_slide.json manifest.json
SLIDE_SIZE=$(wc -c < "on_this_day_slide.wasm")
echo "Done: on_this_day_slide.wasm (${SLIDE_SIZE} bytes)"

echo "Packing on_this_day.vzglyd..."
rm -f on_this_day.vzglyd
zip -X -0 -r on_this_day.vzglyd manifest.json slide.wasm sidecar.wasm assets/ art/
VZGLYD_SIZE=$(wc -c < on_this_day.vzglyd)
echo "Done: on_this_day.vzglyd (${VZGLYD_SIZE} bytes)"
echo "Run with:"
echo "  cargo run --manifest-path ../lume/Cargo.toml -- --scene ../lume-on_this_day"
