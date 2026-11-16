import { HostDevice } from "../web_output/device.js";

const key = "wasm_safe_thread:bootstrap";

function deferred() {
  let resolve;
  const promise = new Promise(done => { resolve = done; });
  return { promise, resolve };
}

function replace(object, name, value) {
  const descriptor = Object.getOwnPropertyDescriptor(object, name);
  object[name] = value;
  return () => {
    if (descriptor) Object.defineProperty(object, name, descriptor);
    else delete object[name];
  };
}

function check(condition, message) {
  if (!condition) throw new Error(message);
}

function scenario(name, timeoutMillis) {
  let stage = "setup";
  const deadline = performance.now() + timeoutMillis;
  let timeoutError;
  const failure = () => timeoutError ??= new Error(`${name} did not complete: stage=${stage}`);
  let timer;
  const expired = new Promise((_, reject) => {
    timer = setTimeout(() => reject(failure()), timeoutMillis);
  });
  return {
    async wait(nextStage, promise) {
      stage = nextStage;
      const result = await Promise.race([promise, expired]);
      if (performance.now() > deadline) throw failure();
      return result;
    },
    async cleanup(nextStage, promises, prior) {
      stage = nextStage;
      const cleanup = scenario("bootstrap cleanup", timeoutMillis);
      try {
        await cleanup.wait(stage, Promise.allSettled(promises));
      } catch (error) {
        if (prior) throw new AggregateError([prior, error], `${prior.message}; ${error.message}`);
        throw error;
      } finally { cleanup.finish(); }
      if (prior) throw prior;
      if (performance.now() > deadline) throw failure();
    },
    finish() { clearTimeout(timer); }
  };
}

async function acquire(deadline) {
  const ready = deferred();
  const done = deferred();
  const abort = new AbortController();
  const held = navigator.locks.request(key, { signal: abort.signal }, () => {
    ready.resolve();
    return done.promise;
  });
  try {
    await deadline.wait("bootstrap admission grant", Promise.race([ready.promise, held]));
    return { held, release: done.resolve };
  } catch (error) {
    abort.abort(error);
    done.resolve();
    await deadline.cleanup("bootstrap admission cancellation", [held], error);
  }
}

export async function holdBootstrap(timeoutMillis) {
  const deadline = scenario("bootstrap lease", timeoutMillis);
  try { return await acquire(deadline); }
  finally { deadline.finish(); }
}

export async function releaseBootstrap(lease, timeoutMillis) {
  const deadline = scenario("bootstrap lease", timeoutMillis);
  try {
    lease.release();
    await deadline.wait("bootstrap admission release", lease.held);
  } finally { deadline.finish(); }
}

async function closedWhileQueued(deadline) {
  const lease = await acquire(deadline);
  const request = navigator.locks.request.bind(navigator.locks);
  const requested = deferred();
  const granted = deferred();
  const module = deferred();
  let imports = 0;
  const restoreContext = replace(globalThis, "AudioContext", class {
    constructor() {
      this.state = "running";
      this.audioWorklet = { addModule() { imports++; return module.promise; } };
    }
    close() { this.state = "closed"; return Promise.resolve(); }
  });
  const restoreRequest = replace(navigator.locks, "request", (name, ...args) => {
    if (name !== key) return request(name, ...args);
    const callback = args.pop();
    const result = request(name, ...args, lock => { granted.resolve(); return callback(lock); });
    requested.resolve();
    return result;
  });
  let successor;
  let device;
  let failure;
  try {
    device = new HostDevice(44100, import.meta.url, "", {}, {}, 2);
    await deadline.wait("closed output request", requested.promise);
    device.close();
    let admitted = false;
    successor = request(key, () => { admitted = true; });
    lease.release();
    await deadline.wait("closed output grant", granted.promise);
    check(imports === 0, "closed queued output must not import into its closed context");
    await deadline.wait("closed output successor", successor);
    check(admitted, "closed output must leave admission available to the next bootstrap");
    check(device.node === null && device.context.state === "closed", "closed output stays inactive");
  } catch (error) {
    failure = error;
  } finally {
    device?.close();
    module.resolve();
    lease.release();
    restoreRequest();
    restoreContext();
    await deadline.cleanup("closed output cleanup", [lease.held, successor], failure);
  }
}

async function liveOutputRetainsAdmissionUntilReady(deadline) {
  const request = navigator.locks.request.bind(navigator.locks);
  const module = deferred();
  const importing = deferred();
  const created = deferred();
  const restoreContext = replace(globalThis, "AudioContext", class {
    constructor() {
      this.state = "running";
      this.audioWorklet = { addModule() { importing.resolve(); return module.promise; } };
    }
    close() { this.state = "closed"; return Promise.resolve(); }
  });
  const restoreNode = replace(globalThis, "AudioWorkletNode", class {
    constructor() {
      this.port = {
        onmessage: null,
        postMessage: ({ kind }) => {
          if (kind === "stop") queueMicrotask(() => this.port.onmessage({ data: { kind: "stopped" } }));
        },
        close() {}
      };
      created.resolve(this);
    }
    disconnect() {}
  });
  let successor;
  let node;
  let device;
  let failure;
  try {
    device = new HostDevice(44100, import.meta.url, "", {}, {}, 2);
    await deadline.wait("worklet import request", importing.promise);
    let admitted = false;
    successor = request(key, () => { admitted = true; });
    await deadline.wait("worklet import lease snapshot", navigator.locks.query());
    check(!admitted, "module import must retain bootstrap admission");
    module.resolve();
    node = await deadline.wait("worklet node construction", created.promise);
    await deadline.wait("worklet construction lease snapshot", navigator.locks.query());
    check(!admitted, "worklet construction must retain bootstrap admission until ready");
    node.port.onmessage({ data: { kind: "ready" } });
    await deadline.wait("ready output successor", successor);
    check(admitted && device.acquired, "ready releases admission for the next bootstrap");
    device.close();
  } catch (error) {
    failure = error;
  } finally {
    device?.close();
    module.resolve();
    restoreNode();
    restoreContext();
    await deadline.cleanup("ready output cleanup", [successor], failure);
  }
}

export async function closedOutputContract(timeoutMillis) {
  const deadline = scenario("closed output bootstrap", timeoutMillis);
  try {
    await closedWhileQueued(deadline);
    await liveOutputRetainsAdmissionUntilReady(deadline);
  } finally { deadline.finish(); }
}
