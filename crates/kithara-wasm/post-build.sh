#!/bin/bash
# Trunk post-build hook: patch wasm-bindgen-rayon for browser compatibility.
set -e

DIR="$TRUNK_STAGING_DIR"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

# 1. Replace workerHelpers.js with our version (logging + timeout + error handling).
#    The generated file is at snippets/wasm-bindgen-rayon-HASH/src/workerHelpers.js.
WORKER_HELPERS=$(find "$DIR/snippets" -name workerHelpers.js 2>/dev/null | head -1)
if [ -n "$WORKER_HELPERS" ]; then
    cp "$SCRIPT_DIR/workerHelpers.js" "$WORKER_HELPERS"
    echo "post-build: replaced $WORKER_HELPERS"
else
    echo "post-build: WARNING — workerHelpers.js not found in snippets/"
fi

# 2. Strip integrity/crossorigin attrs and preload tags from index.html.
#    Trunk adds these but they break under COEP require-corp.
sed -i '' \
    's/ integrity="[^"]*"//g; s/ crossorigin="[^"]*"//g; s/<link rel="modulepreload"[^>]*>//g; s/<link rel="preload"[^>]*>//g' \
    "$DIR/index.html"

echo "post-build: patched index.html"
