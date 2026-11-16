export class OutputStartupProbe {
  constructor() {
    this.contexts = [];
    this.nodes = [];
    this.bootstrap = [];
    this.cleanup = [];
    try {
      const locks = navigator.locks;
      const request = locks.request;
      const bootstrap = this.bootstrap;
      this.replace(locks, "request", function(...args) {
        if (args[0] !== "wasm_safe_thread:bootstrap") return Reflect.apply(request, this, args);
        bootstrap.push("requested");
        const callback = args[args.length - 1];
        args[args.length - 1] = function(...values) {
          bootstrap.push("granted");
          return Reflect.apply(callback, this, values);
        };
        const promise = Reflect.apply(request, this, args);
        promise.then(() => bootstrap.push("released"), () => bootstrap.push("failed"));
        return promise;
      });
      for (const name of ["AudioContext", "AudioWorkletNode"]) {
        const original = globalThis[name];
        this.replace(globalThis, name, new Proxy(original, {
          construct: (target, args, newTarget) => {
            const value = Reflect.construct(target, args, newTarget);
            const events = [];
            if (name === "AudioContext") {
              this.contexts.push({value, events});
              this.listen(value, "statechange", () => events.push(value.state));
            } else {
              this.nodes.push({name: args[1], events});
              this.listen(value.port, "message", ({data}) => events.push(data.kind));
              this.listen(value, "processorerror", () => events.push("processorerror"));
            }
            return value;
          }
        }));
      }
    } catch (error) {
      try { this.restore(); }
      catch (restoreError) { throw new AggregateError([error, restoreError], "Output probe setup failed"); }
      throw error;
    }
  }

  replace(object, name, value) {
    const descriptor = Object.getOwnPropertyDescriptor(object, name);
    this.cleanup.push(() => {
      if (descriptor) Object.defineProperty(object, name, descriptor);
      else delete object[name];
    });
    object[name] = value;
  }

  listen(object, name, listener) {
    object.addEventListener(name, listener);
    this.cleanup.push(() => object.removeEventListener(name, listener));
  }

  async snapshot() {
    const locks = await navigator.locks.query();
    return JSON.stringify({
      contexts: this.contexts.map(({value, events}) => ({state: value.state, time: value.currentTime, rate: value.sampleRate, events})),
      nodes: this.nodes,
      bootstrap: this.bootstrap,
      locks
    });
  }

  restore() {
    const errors = [];
    for (const action of this.cleanup.splice(0).reverse()) {
      try { action(); }
      catch (error) { errors.push(error); }
    }
    if (errors.length) throw new AggregateError(errors, "Output probe restoration failed");
  }
}
