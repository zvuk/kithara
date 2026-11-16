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

export async function holdBootstrap() {
  const ready = deferred();
  const done = deferred();
  const held = navigator.locks.request(key, () => {
    ready.resolve();
    return done.promise;
  });
  await ready.promise;
  return { held, release: done.resolve };
}

export async function releaseBootstrap(lease) {
  lease.release();
  await lease.held;
}

async function closedWhileQueued() {
  const lease = await holdBootstrap();
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
  const restoreRequest = replace(navigator.locks, "request", (name, callback) => {
    const result = request(name, lock => { granted.resolve(); return callback(lock); });
    requested.resolve();
    return result;
  });
  let successor;
  try {
    const device = new HostDevice(44100, import.meta.url, "", {}, {}, 2);
    await requested.promise;
    device.close();
    let admitted = false;
    successor = request(key, () => { admitted = true; });
    lease.release();
    await granted.promise;
    check(imports === 0, "closed queued output must not import into its closed context");
    await successor;
    check(admitted, "closed output must leave admission available to the next bootstrap");
    check(device.node === null && device.context.state === "closed", "closed output stays inactive");
  } finally {
    module.resolve();
    await releaseBootstrap(lease);
    await successor;
    restoreRequest();
    restoreContext();
  }
}

async function liveOutputRetainsAdmissionUntilReady() {
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
  try {
    device = new HostDevice(44100, import.meta.url, "", {}, {}, 2);
    await importing.promise;
    let admitted = false;
    successor = request(key, () => { admitted = true; });
    await navigator.locks.query();
    check(!admitted, "module import must retain bootstrap admission");
    module.resolve();
    node = await created.promise;
    await navigator.locks.query();
    check(!admitted, "worklet construction must retain bootstrap admission until ready");
    node.port.onmessage({ data: { kind: "ready" } });
    await successor;
    check(admitted && device.acquired, "ready releases admission for the next bootstrap");
    device.close();
  } finally {
    module.resolve();
    if (node && !device?.booted) node.port.onmessage({ data: { kind: "ready" } });
    await successor;
    restoreNode();
    restoreContext();
  }
}

export async function closedOutputContract() {
  await closedWhileQueued();
  await liveOutputRetainsAdmissionUntilReady();
}
