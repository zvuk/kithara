
const SELF_URL = import.meta.url;

let __wstCanRelayToParent = false;

// Global cleanup handler, registered once from Rust via register_cleanup_handler()
// Called when exit_state ref_count reaches 0 to free the memory
let __cleanup_handler = null;

export function register_cleanup_handler(handler) {
    __cleanup_handler = handler;
}

// Decrement ref_count and call cleanup if it reaches 0
// exit_state layout: [exit_code: u32, ref_count: u32]
function decrementRefCountAndCleanup(memory, exitStatePtr) {
    const i32 = new Int32Array(memory.buffer);
    const refCountIndex = (exitStatePtr >>> 2) + 1;  // ref_count is at offset 4
    const oldRefCount = Atomics.sub(i32, refCountIndex, 1);
    if (oldRefCount === 1 && __cleanup_handler) {
        // We decremented from 1 to 0, we're the last reference
        __cleanup_handler(exitStatePtr);
    }
}

function isNode() {
  return typeof process !== "undefined" && process.versions && process.versions.node;
}

// Derive the main wasm-bindgen shim URL from our snippet's URL.
// Our inline0.js is at .../snippets/<crate-hash>/inline0.js
// The main shim is at .../<binary-name>.js
function getShimUrl(shimName) {
  const idx = SELF_URL.indexOf("/snippets/");
  if (idx === -1) {
    throw new Error("Cannot derive shim URL: SELF_URL doesn't contain /snippets/: " + SELF_URL);
  }
  const base = SELF_URL.slice(0, idx);
  return base + "/" + shimName + ".js";
}

// Auto-detect the shim URL from loaded resources using Performance API (browser only)
function detectShimUrl() {
  const idx = SELF_URL.indexOf("/snippets/");
  if (idx === -1) {
    throw new Error("Cannot derive shim URL: SELF_URL doesn't contain /snippets/: " + SELF_URL);
  }
  const base = SELF_URL.slice(0, idx);

  // Performance API resource tracking only works in browsers, not Node.js
  if (isNode()) {
    return null;
  }

  // Find loaded JS modules at the base path
  // initiatorType can be 'script', 'module', 'other', etc. depending on browser
  const resources = performance.getEntriesByType('resource');
  for (const r of resources) {
    if (!r.name.startsWith(base + '/')) continue;
    if (r.name.includes('/snippets/')) continue;

    // Extract path without query string
    const url = new URL(r.name);
    const path = url.pathname;

    // Skip run.js (wasm-bindgen-test-runner's test runner)
    if (path.endsWith('/run.js')) continue;

    // Match .js files or extensionless paths (ES module imports)
    // Extensionless: /wasm-bindgen-test, /my-example
    // With extension: /wasm-bindgen-test.js
    if (path.endsWith('.js')) {
      return r.name.split('?')[0];  // Return full URL without query string
    }

    // Also match extensionless imports (Firefox records ES modules without extension)
    // These should be simple names with no extension and no dots
    const filename = path.split('/').pop();
    if (filename && !filename.includes('.') && filename !== 'favicon') {
      return r.name.split('?')[0] + '.js';  // Add .js for consistency
    }
  }

  // Fallback to constructed URL if detection fails
  return null;
}

// For testing: return the detected shim URL (or null if detection fails)
export function get_detected_shim_url() {
  return detectShimUrl();
}

// For debugging: return all resource entries as JSON
export function get_performance_resources_debug() {
  const idx = SELF_URL.indexOf("/snippets/");
  const base = idx === -1 ? null : SELF_URL.slice(0, idx);
  const resources = performance.getEntriesByType('resource');
  return JSON.stringify({
    selfUrl: SELF_URL,
    base: base,
    resources: resources.map(r => ({
      name: r.name,
      initiatorType: r.initiatorType,
      matchesBase: base ? r.name.startsWith(base + '/') : false,
      endsWithJs: r.name.endsWith('.js'),
      hasSnippets: r.name.includes('/snippets/')
    }))
  }, null, 2);
}

// For testing: return the shim URL that would be used for a given shim name
export function get_shim_url_for_testing(shimName) {
  return getShimUrl(shimName);
}

// Browser worker script - uses Web Worker API
function makeBrowserWorkerScript(shimUrl, entryName) {
  return `
    let __wstWorkerId = "unbound";
    let __wstWorkerName = "";
    let __wstExitStatePtr = 0;
    self.onmessage = async (e) => {
      try {
        const [module, memory, work, meta] = e.data;
        if (meta && typeof meta === "object") {
          __wstWorkerId = meta.__wst_id || __wstWorkerId;
          __wstWorkerName = meta.__wst_name || __wstWorkerName;
          __wstExitStatePtr = meta.__wst_exit_state_ptr || __wstExitStatePtr;
          if (meta.__wst_parent_managed === true) {
            globalThis.__wst_can_relay_to_parent = true;
          }
        }

        // Cache-bust to get fresh module state per worker (needed for Safari)
        const url = ${JSON.stringify(shimUrl)} + '?worker=' + Math.random();
        const shim = await import(url);

        // Use initSync with module and memory from main thread
        shim.initSync({ module, memory, thread_stack_size: 1048576 });

        // Call the entry point
        shim[${JSON.stringify(entryName)}](work);

        // Wait for pending async tasks to complete before exiting.
        // Tasks are tracked via task_begin()/task_finished() calls.
        while (true) {
          const pending = shim.wasm_safe_thread_pending_tasks();
          if (pending === 0) break;
          await new Promise(resolve => setTimeout(resolve, 1));
        }

        // Signal exit before closing (browsers have no 'exit' event)
        self.postMessage({
          __wasm_safe_thread_exit: true,
          __wst_id: __wstWorkerId,
          __wst_name: __wstWorkerName,
          __wst_exit_state_ptr: __wstExitStatePtr
        });
        close();
      } catch (err) {
        console.error(err);
        self.postMessage({
          __wasm_safe_thread_error: err.message || String(err),
          __wst_id: __wstWorkerId,
          __wst_name: __wstWorkerName,
          __wst_exit_state_ptr: __wstExitStatePtr
        });
        throw err;
      }
    };
  `;
}

// Node worker script - uses worker_threads API with cache-busting
function makeNodeWorkerScript(shimUrl, entryName) {
  return `
    import { parentPort, threadId } from "node:worker_threads";

    // Cache-bust to get fresh module state per worker
    const url = ${JSON.stringify(shimUrl)} + '?worker=' + threadId;

    // Helper to wait ms milliseconds
    const sleep = (ms) => new Promise(resolve => setTimeout(resolve, ms));

    parentPort.on("message", async (msg) => {
      try {
        const [module, memory, work] = msg;
        const shim = await import(url);

        // initSync is on shim directly (ESM) or shim.default (CJS)
        const initSync = shim.initSync || shim.default?.initSync;
        if (!initSync) throw new Error("No initSync found");

        // thread_stack_size tells runtime this is a worker thread
        initSync({ module, memory, thread_stack_size: 1048576 });

        // Entry point is on shim directly (ESM) or shim.default (CJS)
        const entry = shim[${JSON.stringify(entryName)}] || shim.default?.[${JSON.stringify(entryName)}];
        if (!entry) throw new Error("No entry point found: " + ${JSON.stringify(entryName)});

        entry(work);

        // Wait for pending async tasks to complete before exiting.
        // Tasks are tracked via task_begin()/task_finished() calls.
        const pendingTasks = shim.wasm_safe_thread_pending_tasks || shim.default?.wasm_safe_thread_pending_tasks;
        if (pendingTasks) {
          while (pendingTasks() > 0) {
            await sleep(1);
          }
        }

        parentPort.close();
      } catch (err) {
        console.error(err);
        throw err;
      }
    });
  `;
}

// Helper to signal exit via atomics (works cross-thread)
function signalExit(memory, exitStatePtr, exitCode) {
  const i32 = new Int32Array(memory.buffer);
  const index = exitStatePtr >>> 2;  // Convert byte offset to i32 index
  Atomics.store(i32, index, exitCode);  // 1 = success, 2 = error
  Atomics.notify(i32, index, 1);  // Wake one waiter
}

function wstIsWorkerGlobal() {
  return typeof WorkerGlobalScope !== "undefined" && self instanceof WorkerGlobalScope;
}

function wstCanRelayToParent() {
  if (!__wstCanRelayToParent && typeof globalThis !== "undefined" && globalThis.__wst_can_relay_to_parent === true) {
    __wstCanRelayToParent = true;
  }
  return __wstCanRelayToParent === true;
}

function wstForwardSpawnToParent(payload) {
  try {
    self.postMessage({
      __wst_relay_spawn: true,
      payload,
    });
  } catch (err) {
    signalExit(payload.memory, payload.exitStatePtr, 2);
    decrementRefCountAndCleanup(payload.memory, payload.exitStatePtr);
  }
}

function wstSpawnBrowserWorkerDirect(payload) {
  const { work, module, memory, name, shimUrl, exitStatePtr, workerId, entryName } = payload;
  const src = makeBrowserWorkerScript(shimUrl, entryName);
  const blob = new Blob([src], { type: "text/javascript" });
  const blobUrl = URL.createObjectURL(blob);

  const w = new Worker(blobUrl, { type: "module", name });
  w.onmessage = (e) => {
    const data = e.data;
    if (data && data.__wst_relay_spawn) {
      const relayPayload = data.payload || null;
      if (!relayPayload) {
        return;
      }
      if (wstIsWorkerGlobal() && wstCanRelayToParent()) {
        wstForwardSpawnToParent(relayPayload);
      } else {
        wstSpawnBrowserWorkerDirect({
          work: relayPayload.work,
          module: relayPayload.module,
          memory: relayPayload.memory,
          name: relayPayload.name,
          shimUrl: relayPayload.shimUrl,
          exitStatePtr: relayPayload.exitStatePtr,
          workerId: relayPayload.workerId,
          entryName,
        });
      }
      return;
    }

    if (data && data.__wasm_safe_thread_exit) {
      signalExit(memory, exitStatePtr, 1);
      decrementRefCountAndCleanup(memory, exitStatePtr);
    } else if (data && data.__wasm_safe_thread_error) {
      signalExit(memory, exitStatePtr, 2);
      decrementRefCountAndCleanup(memory, exitStatePtr);
    }
  };

  w.onerror = (e) => {
    console.error("Worker error:", e.message, e.filename, e.lineno);
    signalExit(memory, exitStatePtr, 2);
    decrementRefCountAndCleanup(memory, exitStatePtr);
  };

  w.postMessage([module, memory, work, {
    __wst_id: workerId,
    __wst_name: name,
    __wst_exit_state_ptr: exitStatePtr,
    __wst_parent_managed: true
  }]);
  URL.revokeObjectURL(blobUrl);

  return {
    postMessage: (msg) => w.postMessage(msg),
    terminate: () => w.terminate(),
  };
}

export function wasm_safe_thread_spawn_worker(work, module, memory, name, shimName, exitStatePtr) {
  const workerId = name + '#' + exitStatePtr;
  // Determine shim URL: use explicit name if provided, otherwise auto-detect
  let shimUrl;
  if (shimName) {
    shimUrl = getShimUrl(shimName);
  } else {
    // Try auto-detection, fall back to wasm-bindgen-test for test runner compatibility
    shimUrl = detectShimUrl() || getShimUrl("wasm-bindgen-test");
  }
  const entryName = "wasm_safe_thread_entry_point";

  // Browser: use Web Worker API
  if (typeof Worker === "function" && !isNode()) {
    const payload = {
      work,
      module,
      memory,
      name,
      shimUrl,
      exitStatePtr,
      workerId,
      entryName,
    };
    if (wstIsWorkerGlobal() && wstCanRelayToParent()) {
      wstForwardSpawnToParent(payload);
      return {
        postMessage: (_msg) => {},
        terminate: () => {},
      };
    }
    return wstSpawnBrowserWorkerDirect(payload);
  }

  // Node: use worker_threads (synchronous creation is critical!)
  // If we use an async IIFE, the caller might enter an Atomics.wait loop before
  // the Worker is created, blocking the event loop and preventing the promise from resolving.
  if (isNode()) {
    // Use process.getBuiltinModule for synchronous import (Node.js 20+)
    const wt = process.getBuiltinModule
      ? process.getBuiltinModule("node:worker_threads")
      : null;
    if (!wt) throw new Error("worker_threads not available - need Node.js 20+ for synchronous import");
    const { Worker } = wt;

    const src = makeNodeWorkerScript(shimUrl, entryName);
    const w = new Worker(src, { eval: true, type: "module", name });

    // Note: Both 'error' and 'exit' events fire when a worker throws.
    // We only handle signaling and cleanup in 'exit' to avoid double-decrement of ref_count.
    // The exit code is non-zero when an error occurred.
    w.on('exit', (code) => {
      signalExit(memory, exitStatePtr, code === 0 ? 1 : 2);
      decrementRefCountAndCleanup(memory, exitStatePtr);
    });
    w.on('error', (e) => {
      // Log error for debugging, but don't signal/cleanup here.
      // The 'exit' event always follows and handles cleanup.
      console.error("Worker error:", e);
    });

    w.postMessage([module, memory, work, {
      __wst_id: workerId,
      __wst_name: name,
      __wst_exit_state_ptr: exitStatePtr
    }]);

    return {
      postMessage: (msg) => w.postMessage(msg),
      terminate: () => w.terminate(),
    };
  }

  throw new Error("No Worker API available");
}

// Wait async on an atomic - returns a Promise that resolves when the value changes from 0
// Returns the new value (1 = success, 2 = error)
export function wait_for_exit_async(memory, ptr) {
  const i32 = new Int32Array(memory.buffer);
  const index = ptr >>> 2;

  // Check if already signaled
  const current = Atomics.load(i32, index);
  if (current !== 0) {
    return Promise.resolve(current);
  }

  // Use Atomics.waitAsync if available
  if (typeof Atomics.waitAsync === 'function') {
    const result = Atomics.waitAsync(i32, index, 0);
    if (result.async) {
      return result.value.then(() => Atomics.load(i32, index));
    } else {
      // Already not equal, return current value
      return Promise.resolve(Atomics.load(i32, index));
    }
  }

  // Fallback: poll with setTimeout (for environments without waitAsync)
  return new Promise((resolve) => {
    const poll = () => {
      const val = Atomics.load(i32, index);
      if (val !== 0) {
        resolve(val);
      } else {
        setTimeout(poll, 1);
      }
    };
    poll();
  });
}

