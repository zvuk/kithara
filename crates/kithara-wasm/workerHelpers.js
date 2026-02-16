// Custom workerHelpers.js — replacement for wasm-bindgen-rayon's generated file.
// Adds: logging, timeout, error detection, worker error event handling.
//
// This file is used as BOTH the main-thread module (exports startWorkers)
// and the Worker script (runs the init handler).
//
// IMPORTANT: builder.build() must NOT run on the main thread. On wasm32,
// rayon's wait_until_primed() uses wasm_sync::LockLatch::wait() which
// becomes a tight try_lock spin loop on the main thread (can_block()=false).
// This starves worker threads from acquiring the mutex to set the primed
// latch → deadlock. We offload build() to a dedicated "build coordinator"
// Web Worker where can_block()=true and Condvar::wait() uses real
// Atomics.wait().

// --- Wait for a specific message type, with timeout and error detection ---
function waitForWorkerReady(worker, timeoutMs) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      cleanup();
      reject(new Error(`Worker init timeout after ${timeoutMs}ms`));
    }, timeoutMs);

    function cleanup() {
      clearTimeout(timer);
      worker.removeEventListener('message', onMsg);
      worker.removeEventListener('error', onErr);
    }

    function onMsg({ data }) {
      if (data == null) return;
      if (data.type === 'wasm_bindgen_worker_ready') {
        cleanup();
        resolve();
      } else if (data.type === 'wasm_bindgen_worker_error') {
        cleanup();
        reject(new Error(data.error || 'Worker init failed'));
      }
    }

    function onErr(e) {
      cleanup();
      reject(new Error(`Worker error event: ${e.message || e}`));
    }

    worker.addEventListener('message', onMsg);
    worker.addEventListener('error', onErr);
  });
}

// --- Simple message wait (worker side, no timeout needed) ---
function waitForMsgType(target, type) {
  return new Promise(resolve => {
    target.addEventListener('message', function onMsg({ data }) {
      if (data?.type !== type) return;
      target.removeEventListener('message', onMsg);
      resolve(data);
    });
  });
}

// --- Worker dispatch: handle both rayon-worker and pool-build messages ---
if (typeof self !== 'undefined' && typeof self.document === 'undefined') {
  // We are in a Worker context. Listen for init messages.

  // Handler for rayon worker threads.
  self.addEventListener('message', async function onRayonInit({ data }) {
    if (data?.type !== 'wasm_bindgen_worker_init') return;
    self.removeEventListener('message', onRayonInit);

    try {
      console.log('[worker] init message received, importing module...');
      const pkg = await import('../../../kithara-wasm.js');
      console.log('[worker] module imported, initializing WASM...');
      await pkg.default(data.init);
      console.log('[worker] WASM initialized, sending ready signal');
      postMessage({ type: 'wasm_bindgen_worker_ready' });
      pkg.wbg_rayon_start_worker(data.receiver);
    } catch (e) {
      console.error('[worker] init FAILED:', e);
      try {
        postMessage({ type: 'wasm_bindgen_worker_error', error: String(e) });
      } catch (_) {
        // postMessage might fail if worker is being terminated
      }
    }
  });

  // Handler for pool-build coordinator.
  self.addEventListener('message', async function onPoolBuild({ data }) {
    if (data?.type !== 'wasm_bindgen_pool_build') return;
    self.removeEventListener('message', onPoolBuild);

    try {
      console.log('[build-worker] importing module...');
      const pkg = await import('../../../kithara-wasm.js');
      // pkg.default() returns the raw WASM instance.exports object.
      const wasmExports = await pkg.default(data.init);
      console.log('[build-worker] calling build_global (on worker thread)...');
      // Call the WASM export directly with the raw pointer.
      // On this worker thread, can_block()=true so wait_until_primed()
      // uses real Atomics.wait() instead of a spin loop.
      wasmExports.wbg_rayon_poolbuilder_build(data.builderPtr);
      console.log('[build-worker] build_global done');
      postMessage({ type: 'wasm_bindgen_pool_built' });
    } catch (e) {
      console.error('[build-worker] FAILED:', e);
      postMessage({ type: 'wasm_bindgen_pool_error', error: String(e) });
    }
  });
}

// Firefox GC bug workaround — prevent Worker refs from being collected.
let _workers;

// --- Main thread: create workers and wait for readiness ---
export async function startWorkers(module, memory, builder) {
  const n = builder.numThreads();
  console.log(`[rayon] startWorkers: creating ${n} thread(s)`);

  if (n === 0) {
    throw new Error('num_threads must be > 0.');
  }

  const workerInit = {
    type: 'wasm_bindgen_worker_init',
    init: { module_or_path: module, memory },
    receiver: builder.receiver()
  };

  try {
    _workers = await Promise.all(
      Array.from({ length: n }, async (_, i) => {
        console.log(`[rayon] creating worker ${i}...`);
        const worker = new Worker(new URL('./workerHelpers.js', import.meta.url), {
          type: 'module'
        });
        worker.postMessage(workerInit);
        console.log(`[rayon] waiting for worker ${i} ready (timeout: 10s)...`);
        try {
          await waitForWorkerReady(worker, 10000);
          console.log(`[rayon] worker ${i} ready`);
          return worker;
        } catch (e) {
          console.error(`[rayon] worker ${i} failed:`, e);
          worker.terminate();
          throw e;
        }
      })
    );
  } catch (e) {
    // Terminate any workers that did succeed before the failure
    if (_workers) {
      for (const w of _workers) {
        if (w) w.terminate();
      }
      _workers = null;
    }
    throw e;
  }

  // Offload builder.build() to a dedicated Web Worker to avoid
  // main-thread busy-spin deadlock in wait_until_primed().
  console.log('[rayon] all workers ready, building pool on coordinator worker...');
  const buildWorker = new Worker(new URL('./workerHelpers.js', import.meta.url), {
    type: 'module'
  });
  buildWorker.postMessage({
    type: 'wasm_bindgen_pool_build',
    init: { module_or_path: module, memory },
    builderPtr: builder.__wbg_ptr
  });

  await new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      reject(new Error('Pool build timeout after 10s'));
    }, 10000);

    buildWorker.addEventListener('message', ({ data }) => {
      if (data?.type === 'wasm_bindgen_pool_built') {
        clearTimeout(timer);
        resolve();
      } else if (data?.type === 'wasm_bindgen_pool_error') {
        clearTimeout(timer);
        reject(new Error(data.error || 'Pool build failed'));
      }
    });
    buildWorker.addEventListener('error', (e) => {
      clearTimeout(timer);
      reject(new Error(`Build worker error: ${e.message || e}`));
    });
  });

  buildWorker.terminate();
  console.log('[rayon] thread pool built successfully');
}
