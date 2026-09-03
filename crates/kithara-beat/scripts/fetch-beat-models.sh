#!/usr/bin/env bash
# Fetches the full beat models, which are too large for git. The small model
# and the mel model are committed and need nothing.
set -euo pipefail

BASE="https://github.com/danigb/beat-this-rs/releases/download/model-large"
DIR="$(cd "$(dirname "$0")/.." && pwd)/models"
mkdir -p "$DIR"

curl -fL --retry 3 -o "$DIR/beat_this_full.onnx" "$BASE/beat_this.onnx"
curl -fL --retry 3 -o "$DIR/beat_this_full.onnx.sha256" "$BASE/beat_this.onnx.sha256"
(
    cd "$DIR"
    sed 's/beat_this\.onnx/beat_this_full.onnx/' beat_this_full.onnx.sha256 > checked.sha256
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum -c checked.sha256
    else
        shasum -a 256 -c checked.sha256
    fi
    rm -f beat_this_full.onnx.sha256 checked.sha256
)
echo "Saved $DIR/beat_this_full.onnx"
echo
echo "For the int8 build, quantize it with the upstream script:"
echo "  uv run --with onnx --with onnxruntime \\"
echo "    https://raw.githubusercontent.com/danigb/beat-this-rs/main/scripts/quantize_int8.py \\"
echo "    --input $DIR/beat_this_full.onnx --output $DIR/beat_this_full_int8.onnx"
