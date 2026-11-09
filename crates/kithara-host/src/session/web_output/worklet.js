registerProcessor("KitharaOutput", class extends AudioWorkletProcessor {
  constructor(options) {
    super();
    const [module, memory] = options.processorOptions;
    this.wasm = bindgen.initSync({ module, memory, thread_stack_size: 1048576 });
    this.stopped = false;
    this.processor = null;
    this.port.onmessage = ({ data }) => {
      if (data.kind === "install" && !this.processor) {
        this.processor = bindgen.HostOutput.__wrap(data.handle);
        this.port.postMessage({ kind: "installed" });
      } else if (data.kind === "stop") this.stop();
      else throw new Error("invalid output ownership message");
    };
    this.port.postMessage({ kind: "ready" });
  }

  stop() {
    if (this.stopped) return;
    this.stopped = true;
    if (this.processor) { this.processor.free(); this.processor = null; }
    this.wasm.__wbindgen_thread_destroy(0, 0, 0);
    this.port.postMessage({ kind: "stopped" });
  }

  process(inputs, outputs) {
    if (this.stopped) return false;
    if (!this.processor) return true;
    try {
      const running = this.processor.process(outputs[0], currentTime);
      if (!running) this.stop();
      return running;
    } catch (error) {
      this.port.postMessage({ kind: "error", message: String(error) });
      this.stop();
      return false;
    }
  }
});
