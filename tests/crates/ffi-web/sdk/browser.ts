import { run } from "./contract";
import { waitForWorker } from "./generated/kithara_config_uniffi_probe";

async function main() {
  const main = await run();
  const worker = new Worker("/worker.js", { type: "module" });
  try {
    const result = await new Promise<{revision: bigint, memory: number}>((resolve, reject) => {
      worker.onmessage = event => event.data.error ? reject(new Error(event.data.error)) : resolve(event.data.result);
      worker.onerror = event => reject(new Error(event.message));
    });
    if (result.revision !== main.revision) throw new Error("worker exact u64");
    document.body.textContent = `RUNNING main+worker exact-u64 optional nested enum defaults async-error handles callbacks; memory=${main.memory}/${result.memory}`;
  } finally {
    worker.terminate();
  }
  const producer = new Worker("/wake-worker.js", { type: "module" });
  try {
    let callback: bigint | undefined;
    const pending = waitForWorker({ changed: value => { callback = value; } });
    const released = new Promise<void>((resolve, reject) => {
      producer.onmessage = event => event.data.released ? resolve() : reject(new Error(event.data.error ?? "no Rust sender"));
      producer.onerror = event => reject(new Error(event.message));
    });
    producer.postMessage(main.shared);
    await released;
    const value = await pending;
    if (value !== main.revision || callback !== value) throw new Error("cross-thread wake/callback affinity");
    document.body.textContent = document.body.textContent!.replace("RUNNING", "PASS") + "; cross-thread-wake PASS";
  } finally { producer.terminate(); }
}
main().catch(error => { document.body.textContent = `FAIL ${error.stack ?? error}`; });
