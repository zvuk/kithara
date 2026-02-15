// Custom workerHelpers.js — replacement for wasm-bindgen-rayon's generated file.
// Adds: logging, timeout, error detection, worker error event handling.
//
// This file is used as BOTH the main-thread module (exports startWorkers)
// and the Worker script (runs the init handler).

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

// --- Worker init handler (runs in Worker context) ---
waitForMsgType(self, 'wasm_bindgen_worker_init').then(async ({ init, receiver }) => {
  console.log('[worker] init message received, importing module...');
  const pkg = await import('../../../kithara-wasm.js');
  console.log('[worker] module imported, initializing WASM...');
  await pkg.default(init);
  console.log('[worker] WASM initialized, sending ready signal');
  postMessage({ type: 'wasm_bindgen_worker_ready' });
  pkg.wbg_rayon_start_worker(receiver);
}).catch(e => {
  console.error('[worker] init FAILED:', e);
  try {
    postMessage({ type: 'wasm_bindgen_worker_error', error: String(e) });
  } catch (_) {
    // postMessage might fail if worker is being terminated
  }
});

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

  console.log('[rayon] all workers ready, calling builder.build()');
  builder.build();
  console.log('[rayon] thread pool built successfully');
}
