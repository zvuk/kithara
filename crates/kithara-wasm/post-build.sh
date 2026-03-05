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

# Disable cleanup handler entirely.
# Worker __wasm_init_tls corrupts WASM memory; the cleanup callback (a
# wasm-bindgen closure) references corrupted data and crashes with
# "index out of bounds". The cleanup only frees 8 bytes per thread
# (exit_state: [exit_code: u32, ref_count: u32]) — acceptable leak
# for the lifetime of the tab.
for F in "$DIR"/snippets/wasm_safe_thread-*/inline0.js; do
    if [ -f "$F" ]; then
        sedi 's/__cleanup_handler(exitStatePtr);/\/* cleanup disabled: 8-byte leak per thread *\//' "$F"
        echo "post-build: cleanup handler disabled (8-byte leak per thread)"
    fi
done

# Fix wasm_safe_thread double-decrement bug in Worker error handler.
# When a Worker catches an error, the script does postMessage(error) + throw err.
# The throw causes the parent's onerror to fire IN ADDITION to onmessage,
# decrementing ref_count twice → use-after-free → "index out of bounds" crash.
# Fix: replace `throw err` with explicit worker shutdown. This preserves
# single-path error reporting (message only) and avoids leaving errored
# workers alive.
for F in "$DIR"/snippets/wasm_safe_thread-*/inline0.js; do
    if [ -f "$F" ]; then
        sedi 's/throw err;$/if (typeof close === \"function\") { close(); }/' "$F"
        echo "post-build: wasm_safe_thread double-decrement fix applied"
    fi
done

# Serialize Worker initSync calls via Atomics boot lock.
# Prevents concurrent dlmalloc access during TLS initialization which
# corrupts shared WASM heap metadata. The boot lock address (a static
# AtomicI32 in shared WASM memory) is stored in globalThis.__wst_boot_lock_ptr
# by player_new() and passed to Workers via postMessage metadata.
# Workers acquire the lock with Atomics.compareExchange before initSync
# and release with Atomics.store + Atomics.notify after.
for F in "$DIR"/snippets/wasm_safe_thread-*/inline0.js; do
    if [ -f "$F" ]; then
        # 1. Inject boot lock helper function into browser Worker script template.
        #    Placed after __wstExitStatePtr declaration, before self.onmessage.
        sedi 's/let __wstExitStatePtr = 0;/let __wstExitStatePtr = 0; function __wstLockedInit(s,mod,mem,m){const bl=(m\&\&m.__wst_boot_lock_ptr)||0;if(bl>0){const li=new Int32Array(mem.buffer),lx=bl>>>2;while(Atomics.compareExchange(li,lx,0,1)!==0)Atomics.wait(li,lx,1,10);}try{s.initSync({module:mod,memory:mem,thread_stack_size:1048576});}finally{if(bl>0){const li2=new Int32Array(mem.buffer),lx2=bl>>>2;Atomics.store(li2,lx2,0);Atomics.notify(li2,lx2);}}}/' "$F"
        # 2. Replace initSync call with locked version in browser Worker template.
        sedi 's/shim\.initSync({ module, memory, thread_stack_size: 1048576 });/__wstLockedInit(shim, module, memory, meta);/' "$F"
        # 3. Pass boot lock pointer to Workers via postMessage metadata.
        sedi 's/__wst_parent_managed: true/__wst_parent_managed: true, __wst_boot_lock_ptr: (globalThis.__wst_boot_lock_ptr || 0)/' "$F"
        echo "post-build: boot lock serialization applied"
    fi
done

# ----------- Append runtime helpers / optional Player wrapper -----------
# Always appends checkRuntime(); appends Player wrapper only for free-function
# exports (`player_*`) when wasm-bindgen class `Player` is not present.
if [ -f "$JS" ]; then
    {
        echo ""
        echo "// --- Auto-generated by post-build.sh ---"
        echo ""
        cat << 'RUNTIME_CHECK'
/**
 * Check if the browser environment supports SharedArrayBuffer + COEP/COOP.
 * Returns { ok: boolean, reason?: string, waitingForReload?: boolean }.
 *
 * When `waitingForReload` is true, the COI service worker is registering
 * and the page will auto-reload once it activates. The caller should show
 * a "please wait" message and stop initialisation.
 */
export function checkRuntime() {
    const crossOriginIsolated = self.crossOriginIsolated === true;
    const secureContext = self.isSecureContext === true;
    const sharedArrayBuffer = typeof SharedArrayBuffer !== 'undefined';

    if (secureContext && sharedArrayBuffer && crossOriginIsolated) {
        sessionStorage.removeItem('kithara_coi_reloaded');
        return { ok: true };
    }

    const waitingForReload =
        secureContext && !crossOriginIsolated &&
        typeof navigator.serviceWorker !== 'undefined' &&
        !navigator.serviceWorker.controller;

    if (waitingForReload) {
        navigator.serviceWorker.ready.then(() => {
            if (navigator.serviceWorker.controller || self.crossOriginIsolated === true) {
                sessionStorage.removeItem('kithara_coi_reloaded');
                return;
            }
            if (sessionStorage.getItem('kithara_coi_reloaded') === '1') return;
            sessionStorage.setItem('kithara_coi_reloaded', '1');
            window.location.reload();
        }).catch(() => {});
        return { ok: false, waitingForReload: true, reason: 'Waiting for COI service worker to activate' };
    }

    return {
        ok: false,
        waitingForReload: false,
        reason: `secureContext=${secureContext} crossOriginIsolated=${crossOriginIsolated} sharedArrayBuffer=${sharedArrayBuffer}`,
    };
}
RUNTIME_CHECK
    } >> "$JS"
    if grep -qE '^export function player_new\(' "$JS" && ! grep -qE '^export class Player' "$JS"; then
        {
            echo ""
            echo "export class Player {"

            grep -oE 'export function player_[a-z_]+\([^)]*\)' "$JS" \
            | sort \
            | while IFS= read -r sig; do
                fname=$(echo "$sig" | sed 's/export function \([a-z_]*\).*/\1/')
                params=$(echo "$sig" | sed 's/[^(]*(\([^)]*\)).*/\1/')
                method=$(echo "$fname" | sed 's/^player_//')

                if [ "$method" = "new" ]; then
                    echo "    constructor(readyPromise) { this._ready = readyPromise; }"
                    echo "    static async create() { const p = player_new(); const inst = new Player(p); await p; return inst; }"
                elif [ -z "$params" ]; then
                    echo "    ${method}() { return ${fname}(); }"
                else
                    echo "    ${method}(${params}) { return ${fname}(${params}); }"
                fi
            done

            echo "}"
        } >> "$JS"
        echo "post-build: Player class appended to kithara-wasm.js"
    else
        echo "post-build: Player class append skipped (native wasm-bindgen Player export detected)"
    fi
fi

echo "post-build: done"
