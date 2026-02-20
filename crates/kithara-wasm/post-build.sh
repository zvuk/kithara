#!/bin/bash
# Trunk post-build hook: patch for COEP compatibility and worker module loading.
set -e

DIR="$TRUNK_STAGING_DIR"

# Strip integrity/crossorigin attrs and preload tags from index.html.
# Trunk adds these but they break under COEP require-corp.
sed -i '' \
    's/ integrity="[^"]*"//g; s/ crossorigin="[^"]*"//g; s/<link rel="modulepreload"[^>]*>//g; s/<link rel="preload"[^>]*>//g' \
    "$DIR/index.html"

# Trunk injects absolute wasm loader path (`/kithara-wasm.js`) in generated
# bootstrap script. Rewrite to relative path so gh-pages subpath works.
sed -i '' \
    "s|from '/kithara-wasm.js'|from './kithara-wasm.js'|g" \
    "$DIR/index.html"
sed -i '' \
    "s|module_or_path: '/kithara-wasm_bg.wasm'|module_or_path: './kithara-wasm_bg.wasm'|g" \
    "$DIR/index.html"

echo "post-build: done"
