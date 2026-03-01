#!/bin/bash
# Trunk post-build hook: patch for COEP compatibility and worker module loading.
set -e

DIR="$TRUNK_STAGING_DIR"

# Cross-platform sed in-place: macOS requires `sed -i ''`, Linux requires `sed -i`.
sedi() {
    if [[ "$OSTYPE" == darwin* ]]; then
        sed -i '' "$@"
    else
        sed -i "$@"
    fi
}

# Strip integrity/crossorigin attrs and preload tags from index.html.
# Trunk adds these but they break under COEP require-corp.
sedi \
    's/ integrity="[^"]*"//g; s/ crossorigin="[^"]*"//g; s/<link rel="modulepreload"[^>]*>//g; s/<link rel="preload"[^>]*>//g' \
    "$DIR/index.html"

# Trunk injects absolute wasm loader path (`/kithara-wasm.js`) in generated
# bootstrap script. Rewrite to relative path so gh-pages subpath works.
sedi \
    "s|from '/kithara-wasm.js'|from './kithara-wasm.js'|g" \
    "$DIR/index.html"
sedi \
    "s|module_or_path: '/kithara-wasm_bg.wasm'|module_or_path: './kithara-wasm_bg.wasm'|g" \
    "$DIR/index.html"

# Polyfill TextDecoder/TextEncoder for AudioWorkletGlobalScope.
# Some browsers don't expose TextDecoder in AudioWorklet, causing
# wasm-bindgen's cachedTextDecoder to be undefined.
JS="$DIR/kithara-wasm.js"
if [ -f "$JS" ]; then
    sedi 's/^let cachedTextDecoder/if(typeof TextDecoder==="undefined"){globalThis.TextDecoder=class{constructor(){}decode(b){if(!b||!b.length)return"";let r="";for(let i=0;i<b.length;i++)r+=String.fromCharCode(b[i]);return r}};globalThis.TextEncoder=class{constructor(){}encode(s){const a=new Uint8Array(s.length);for(let i=0;i<s.length;i++)a[i]=s.charCodeAt(i);return a}encodeInto(s,d){const e=this.encode(s);d.set(e);return{read:s.length,written:e.length}}}}\
let cachedTextDecoder/' "$JS"
    echo "post-build: TextDecoder polyfill applied"
fi

echo "post-build: done"
