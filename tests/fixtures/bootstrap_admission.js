const key = "wasm_safe_thread:bootstrap";
let held;
let release;
let originalRequest;
let descriptor;
let moduleCalls = 0;
let requests = 0;
const worklets = [];
const contexts = [];

export async function holdBootstrapImports() {
    const locks = navigator.locks;
    originalRequest = locks.request;
    await new Promise(acquired => {
        held = originalRequest.call(locks, key, async () => {
            const gate = new Promise(resolve => { release = resolve; });
            acquired();
            await gate;
        });
    });
    locks.request = function (...args) {
        if (args[0] === key) requests++;
        return originalRequest.apply(this, args);
    };
    descriptor = Object.getOwnPropertyDescriptor(BaseAudioContext.prototype, "audioWorklet");
    Object.defineProperty(BaseAudioContext.prototype, "audioWorklet", {
        ...descriptor,
        get() {
            const worklet = descriptor.get.call(this);
            if (!worklets.includes(worklet)) {
                const original = worklet.addModule;
                worklet.addModule = function (...args) {
                    moduleCalls++;
                    return original.apply(this, args);
                };
                worklets.push(worklet);
                contexts.push(this);
            }
            return worklet;
        },
    });
}

export function workletImports() { return moduleCalls; }
export function bootstrapRequests() { return requests; }
export async function bootstrapLocks() {
    const snapshot = await navigator.locks.query();
    return JSON.stringify({
        held: snapshot.held.filter(lock => lock.name === key),
        pending: snapshot.pending.filter(lock => lock.name === key),
    });
}
export function audioContexts() {
    return JSON.stringify(contexts.map(context => ({
        state: context.state,
        currentTime: context.currentTime,
        sampleRate: context.sampleRate,
    })));
}

export async function releaseBootstrapImports() {
    navigator.locks.request = originalRequest;
    Object.defineProperty(BaseAudioContext.prototype, "audioWorklet", descriptor);
    for (const worklet of worklets) delete worklet.addModule;
    release();
    await held;
}
