
export function is_node() {
  return typeof process !== "undefined" &&
    process?.versions?.node != null &&
    process?.release?.name === "node";
}

export function is_main_thread() {
  if (!is_node()) {
    // Browser main thread (classic window context)
    if (typeof window !== "undefined" && typeof document !== "undefined") {
      return true;
    }
    return false;
  }

  // Node: synchronous main-thread detection (ESM-safe)
  if (process?.getBuiltinModule) {
    const wt = process.getBuiltinModule("node:worker_threads");
    if (wt && typeof wt.isMainThread === "boolean") {
      return wt.isMainThread;
    }
  }

  throw new Error("Can't detect");

}

let __yield_sab = new SharedArrayBuffer(4);
let __yield_i32 = new Int32Array(__yield_sab);

// Yields to the browser event loop using setTimeout.
// Returns a Promise that resolves after the event loop processes.
// Note: We use 1ms instead of 0 because Chrome batches zero-delay timeouts
// aggressively, which may not give workers enough time to execute.
export async function yield_to_event_loop() {
  return new Promise(resolve => setTimeout(resolve, 1));
}

// Returns one of:
//   "ok" | "not-equal" | "timed-out" | "unsupported"
export function atomics_wait_timeout_ms_try(timeout_ms) {
  try {
    return Atomics.wait(__yield_i32, 0, 0, timeout_ms);
  } catch (_e) {
    // e.g. browser window main thread, or environments without SAB/Atomics.wait
    return "unsupported";
  }
}

export function sleep_sync_ms(ms) {
  if (ms <= 0) return;

  try {
    // SharedArrayBuffer may be unavailable unless crossOriginIsolated (in browsers)
    const sab = new SharedArrayBuffer(4);
    const i32 = new Int32Array(sab);
    // Wait while i32[0] is 0, with a timeout (ms). Returns "timed-out" typically.
    Atomics.wait(i32, 0, 0, ms);
    return;
  } catch {
    // Fall through to the worst-case synchronous fallback below.
  }

  // Worst-case fallback: busy-wait (CPU burn). Still fully synchronous.
  const end = (typeof performance !== "undefined" && performance.now)
    ? performance.now() + ms
    : Date.now() + ms;

  if (typeof performance !== "undefined" && performance.now) {
    while (performance.now() < end) {}
  } else {
    while (Date.now() < end) {}
  }
}

// Parks at a memory address within wasm linear memory.
// ptr is a byte offset into wasm memory, must be 4-byte aligned.
// Returns "ok" if woken by notify, "timed-out", or "unsupported"
export function park_wait_at_addr(memory, ptr) {
  try {
    const i32 = new Int32Array(memory.buffer);
    const index = ptr >>> 2;  // Convert byte offset to i32 index
    // Try to consume an existing token (1 -> 0)
    if (Atomics.compareExchange(i32, index, 1, 0) === 1) {
      return "ok";  // Had a pending unpark token
    }
    // Wait only if value is 0 (no token) - this is atomic with the check
    const result = Atomics.wait(i32, index, 0);
    // After wakeup, consume the token
    Atomics.store(i32, index, 0);
    return result;
  } catch (_e) {
    return "unsupported";
  }
}

// Parks with timeout at a memory address.
export function park_wait_timeout_at_addr(memory, ptr, timeout_ms) {
  try {
    const i32 = new Int32Array(memory.buffer);
    const index = ptr >>> 2;
    // Try to consume an existing token (1 -> 0)
    if (Atomics.compareExchange(i32, index, 1, 0) === 1) {
      return "ok";  // Had a pending unpark token
    }
    // Wait only if value is 0 (no token) - this is atomic with the check
    const result = Atomics.wait(i32, index, 0, timeout_ms);
    if (result === "ok") {
      // Woken by notify, consume the token
      Atomics.store(i32, index, 0);
    }
    return result;
  } catch (_e) {
    return "unsupported";
  }
}

// Unparks a thread by setting its token and notifying at a memory address.
export function park_notify_at_addr(memory, ptr) {
  const i32 = new Int32Array(memory.buffer);
  const index = ptr >>> 2;
  Atomics.store(i32, index, 1);   // Set token
  Atomics.notify(i32, index, 1);  // Wake one waiter
}

// Returns the number of logical processors available, or 1 if unknown.
export function get_available_parallelism() {
  // Browser: navigator.hardwareConcurrency
  if (typeof navigator !== "undefined" && navigator.hardwareConcurrency) {
    return navigator.hardwareConcurrency;
  }

  // Node.js: try os.availableParallelism() (Node 19.4+) or os.cpus().length
  if (typeof process !== "undefined" && process.versions && process.versions.node) {
    try {
      const os = process.getBuiltinModule?.("node:os");
      if (os) {
        // Node 19.4+ has availableParallelism()
        if (typeof os.availableParallelism === "function") {
          return os.availableParallelism();
        }
        // Fallback to cpus().length
        const cpus = os.cpus?.();
        if (cpus && cpus.length > 0) {
          return cpus.length;
        }
      }
    } catch {
      // Ignore errors
    }
  }

  // Unknown environment
  return 1;
}
