export class HostDevice {
  constructor(rate, root, script, module, memory, channels) {
    this.context = new AudioContext({ sampleRate: rate });
    this.owner = null;
    this.node = null;
    this.sent = false;
    this.connected = false;
    this.closed = false;
    this.failure = undefined;
    this.listeners = [];
    this.initialize(root, script, module, memory, channels).catch((error) => this.fail(error));
  }

  get sampleRate() { return this.context.sampleRate; }
  get acquired() { return !!this.booted && !this.closed && !this.failure; }
  get currentTime() { return this.context.currentTime; }
  get ready() { return this.connected && !this.closed && !this.failure && this.context.state !== "closed"; }
  get error() { return this.failure; }

  async initialize(root, script, module, memory, channels) {
    this.bindings = await import(root);
    const url = URL.createObjectURL(new Blob([
      `import * as bindgen from ${JSON.stringify(root)};\n`, script,
    ], { type: "text/javascript" }));
    try {
      await navigator.locks.request("wasm_safe_thread:bootstrap", () => this.context.audioWorklet.addModule(url));
    } finally {
      URL.revokeObjectURL(url);
    }
    if (this.closed) return;
    this.node = new AudioWorkletNode(this.context, "KitharaOutput", {
      numberOfInputs: 0, numberOfOutputs: 1,
      channelCount: channels, outputChannelCount: [channels],
      processorOptions: [module, memory],
    });
    this.node.onprocessorerror = (event) => this.fail(event.message);
    this.node.port.onmessage = ({ data }) => {
      switch (data.kind) {
        case "ready":
          this.booted = true;
          if (this.closed) this.node.port.postMessage({ kind: "stop" });
          else this.transfer();
          break;
        case "installed":
          if (this.closed) { this.node.port.postMessage({ kind: "stop" }); break; }
          try {
            this.node.connect(this.context.destination);
            this.connected = true;
            this.enableResume();
          } catch (error) { this.fail(error); }
          break;
        case "error": this.fail(data.message); break;
        case "stopped":
          this.sent = false;
          this.connected = false;
          this.closed = true;
          this.removeListeners();
          this.node.disconnect();
          this.node.port.close();
          this.closeContext();
          break;
        default: this.fail("invalid output worklet message");
      }
    };
  }

  install(owner) {
    if (this.closed || this.failure || this.owner || this.sent) {
      owner.free();
      throw new Error(this.failure || "output installation is not vacant");
    }
    this.owner = owner;
    this.transfer();
  }

  transfer() {
    if (!this.booted || !this.owner || this.closed) return;
    const handle = this.owner.__destroy_into_raw();
    this.owner = null;
    try {
      this.node.port.postMessage({ kind: "install", handle });
      this.sent = true;
    } catch (error) {
      this.bindings.HostOutput.__wrap(handle).free();
      this.fail(error);
    }
  }

  enableResume() {
    if (this.context.state === "running") return;
    const resume = () => {
      this.context.resume().then(() => this.removeListeners()).catch((error) => console.error("Audio autoresume failed", error));
    };
    for (const name of ["click", "contextmenu", "auxclick", "dblclick", "mousedown", "mouseup", "pointerup", "touchend", "keydown", "keyup"]) {
      document.addEventListener(name, resume);
      this.listeners.push([name, resume]);
    }
  }

  removeListeners() {
    for (const [name, listener] of this.listeners) document.removeEventListener(name, listener);
    this.listeners.length = 0;
  }

  fail(error) {
    this.failure = String(error);
    this.close();
  }

  closeContext() {
    if (this.context.state !== "closed") this.context.close().catch((error) => console.error("Audio context close failed", error));
  }

  close() {
    if (this.closed) return;
    this.closed = true;
    this.removeListeners();
    if (this.owner) { this.owner.free(); this.owner = null; }
    if (this.node) this.node.port.postMessage({ kind: "stop" });
    else this.closeContext();
  }
}
