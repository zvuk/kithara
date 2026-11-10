const key = "wasm_safe_thread:bootstrap";
let held;
let release;
let originalRequest;
let descriptor;
let moduleCalls = 0;
let requests = 0;
const bootstrap = [];
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
        if (args[0] !== key) return originalRequest.apply(this, args);
        requests++;
        const callback = args[args.length - 1];
        args[args.length - 1] = function (...callbackArgs) {
            bootstrap.push({ phase: "lock-granted" });
            return callback.apply(this, callbackArgs);
        };
        const promise = originalRequest.apply(this, args);
        promise.then(
            () => bootstrap.push({ phase: "lock-resolved" }),
            error => bootstrap.push({ phase: "lock-rejected", error: String(error) }),
        );
        return promise;
    };
    descriptor = Object.getOwnPropertyDescriptor(BaseAudioContext.prototype, "audioWorklet");
    Object.defineProperty(BaseAudioContext.prototype, "audioWorklet", {
        ...descriptor,
        get() {
            const worklet = descriptor.get.call(this);
            if (!contexts.some(entry => entry.worklet === worklet)) {
                const entry = { context: this, worklet, transitions: [], restore: [] };
                for (const [receiver, name] of [[worklet, "addModule"], [this, "resume"]]) {
                    const own = Object.getOwnPropertyDescriptor(receiver, name);
                    const original = receiver[name];
                    entry.restore.push(() => {
                        if (own) Object.defineProperty(receiver, name, own);
                        else delete receiver[name];
                    });
                    receiver[name] = function (...args) {
                        if (name === "addModule") moduleCalls++;
                        entry.transitions.push({ method: name, phase: "called" });
                        try {
                            const promise = original.apply(this, args);
                            promise.then(
                                () => entry.transitions.push({ method: name, phase: "resolved" }),
                                error => entry.transitions.push({ method: name, phase: "rejected", error: String(error) }),
                            );
                            return promise;
                        } catch (error) {
                            entry.transitions.push({ method: name, phase: "threw", error: String(error) });
                            throw error;
                        }
                    };
                }
                contexts.push(entry);
            }
            return worklet;
        },
    });
}

export function workletImports() { return moduleCalls; }
export function bootstrapRequests() { return requests; }
export function audioContexts() {
    try {
        return JSON.stringify(contexts.map(({ context, transitions }) => ({
            state: context.state,
            currentTime: context.currentTime,
            sampleRate: context.sampleRate,
            bootstrap,
            transitions,
        })));
    } finally {
        for (const entry of contexts) {
            for (const restore of entry.restore) restore();
        }
    }
}

export async function releaseBootstrapImports() {
    navigator.locks.request = originalRequest;
    Object.defineProperty(BaseAudioContext.prototype, "audioWorklet", descriptor);
    release();
    await held;
}
