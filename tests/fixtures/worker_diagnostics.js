const observations = [];

export function installWorkerDiagnostics() {
    const NativeWorker = globalThis.Worker;
    const channel = new BroadcastChannel("kithara-worker-diagnostics");
    const record = observation => {
        observations.push(observation);
        channel.postMessage(observation);
    };
    globalThis.Worker = new Proxy(NativeWorker, {
        construct(target, args, newTarget) {
            const worker = Reflect.construct(target, args, newTarget);
            const url = String(args[0]);
            const name = args[1]?.name ?? null;
            record({ kind: "created", url, name });
            worker.addEventListener("message", event => {
                const data = event.data;
                if (data?.__wasm_safe_thread_exit || data?.__wasm_safe_thread_error) {
                    record({
                        kind: data.__wasm_safe_thread_exit ? "exit" : "worker_error",
                        url, name, workerId: data.__wst_id,
                        message: data.__wasm_safe_thread_error ?? null,
                    });
                }
            });
            worker.addEventListener("error", event => record({
                kind: "error", url, name, message: event.message,
                filename: event.filename, line: event.lineno, column: event.colno,
            }));
            worker.addEventListener("messageerror", () => record({
                kind: "messageerror", url, name,
            }));
            return worker;
        },
    });
}

export function reportWorkerDiagnostics() {
    return JSON.stringify(observations);
}

export function checkWorkerConstructorForwarding() {
    const worker = globalThis.Worker;
    const channel = globalThis.BroadcastChannel;
    const recorded = observations.length;
    const constructed = [];
    const failure = new Error("constructor failure");
    class NativeWorker {
        static marker = "worker-constructor";
        constructor(...args) {
            if (args[0] === failure) throw failure;
            constructed.push(args);
        }
        addEventListener() {}
    }
    function RunnerWorker(...args) {
        if (typeof args[0] === "string") args[0] = "rewritten:" + args[0];
        return Reflect.construct(NativeWorker, args);
    }
    RunnerWorker.prototype = NativeWorker.prototype;
    try {
        globalThis.BroadcastChannel = class { postMessage() {} };
        for (const constructor of [NativeWorker, RunnerWorker]) {
            const options = { type: "module", name: "constructor-contract" };
            const original = new constructor("blob:original-worker", options);
            const expected = constructed.at(-1);
            globalThis.Worker = constructor;
            installWorkerDiagnostics();
            const instance = new globalThis.Worker("blob:original-worker", options);
            const actual = constructed.at(-1);
            if (actual.length !== expected.length ||
                actual.some((value, index) => value !== expected[index])) {
                throw new Error("worker observer changed constructor arguments");
            }
            if (Object.getPrototypeOf(instance) !== Object.getPrototypeOf(original) ||
                !(instance instanceof globalThis.Worker)) {
                throw new Error("worker observer changed the instance prototype");
            }
            if (globalThis.Worker.marker !== constructor.marker) {
                throw new Error("worker observer changed constructor properties");
            }
            new globalThis.Worker("blob:single-argument");
            if (constructed.at(-1).length !== 1) {
                throw new Error("worker observer changed the argument count");
            }
            try {
                new globalThis.Worker(failure);
                throw new Error("worker observer swallowed constructor failure");
            } catch (error) {
                if (error !== failure) throw error;
            }
        }
    } finally {
        globalThis.Worker = worker;
        globalThis.BroadcastChannel = channel;
        observations.length = recorded;
    }
}
