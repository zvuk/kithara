
// Returns a Promise that resolves when the i32 at `ptr` changes from 0.
export function atomics_wait_async(memory, ptr) {
  const i32 = new Int32Array(memory.buffer);
  const index = ptr >>> 2;
  const current = Atomics.load(i32, index);
  if (current !== 0) {
    return Promise.resolve(current);
  }
  if (typeof Atomics.waitAsync === 'function') {
    const result = Atomics.waitAsync(i32, index, 0);
    if (result.async) {
      return result.value.then(() => Atomics.load(i32, index));
    }
    return Promise.resolve(Atomics.load(i32, index));
  }
  // Fallback: poll with setTimeout
  return new Promise((resolve) => {
    const poll = () => {
      const val = Atomics.load(i32, index);
      if (val !== 0) resolve(val);
      else setTimeout(poll, 1);
    };
    poll();
  });
}

// Stores 1 at `ptr` and wakes one waiter via Atomics.notify.
export function atomics_notify(memory, ptr) {
  const i32 = new Int32Array(memory.buffer);
  const index = ptr >>> 2;
  Atomics.store(i32, index, 1);
  Atomics.notify(i32, index, 1);
}
