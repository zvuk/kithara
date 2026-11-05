const observations = [];

export function installWorkerDiagnostics() {
    const NativeWorker = globalThis.Worker;
    const channel = new BroadcastChannel("kithara-worker-diagnostics");
    const record = observation => {
        observations.push(observation);
        channel.postMessage(observation);
    };
    globalThis.Worker = class Worker extends NativeWorker {
        constructor(...args) {
            super(...args);
            const url = String(args[0]);
            const name = args[1]?.name ?? null;
            record({ kind: "created", url, name });
            this.addEventListener("message", event => {
                const data = event.data;
                if (data?.__wasm_safe_thread_exit || data?.__wasm_safe_thread_error) {
                    record({
                        kind: data.__wasm_safe_thread_exit ? "exit" : "worker_error",
                        url, name, workerId: data.__wst_id,
                        message: data.__wasm_safe_thread_error ?? null,
                    });
                }
            });
            this.addEventListener("error", event => record({
                kind: "error", url, name, message: event.message,
                filename: event.filename, line: event.lineno, column: event.colno,
            }));
            this.addEventListener("messageerror", () => record({
                kind: "messageerror", url, name,
            }));
        }
    };
}

export function reportWorkerDiagnostics() {
    return JSON.stringify(observations);
}
