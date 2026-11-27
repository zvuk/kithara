import {
    WasmBindgenTestContext as Context,
    __wbgtest_console_debug,
    __wbgtest_console_log,
    __wbgtest_console_info,
    __wbgtest_console_warn,
    __wbgtest_console_error,
    default as init,
} from "./wasm-bindgen-test.js";

// Workers the tests spawn log into their own realm. Each one is started from a
// wrapper that installs this shim first, so its console reaches the page too.
function __wbg_install_worker_console_shim(sendToParent) {
    if (self.__wbg_worker_console_shim_installed) {
        return;
    }
    self.__wbg_worker_console_shim_installed = true;

    function forward(send, method, args) {
        try {
            send(["__wbgtest_" + method, args]);
        } catch (e) {
            try {
                send(["__wbgtest_" + method, args.map(String)]);
            } catch (e2) {}
        }
    }

    function relay(e) {
        if (Array.isArray(e.data) && typeof e.data[0] === "string" && e.data[0].startsWith("__wbgtest_")) {
            forward(sendToParent, e.data[0].slice(10), e.data[1]);
            e.stopImmediatePropagation();
        }
    }

    for (const method of ["debug", "log", "info", "warn", "error"]) {
        const original = console[method];
        console[method] = function (...args) {
            original.apply(this, args);
            forward(sendToParent, method, args);
        };
    }

    if (typeof Worker === "function") {
        const OriginalWorker = Worker;
        Worker = function (url, options) {
            const wrapped = __wbg_wrap_worker_url(url, options, __wbg_install_worker_console_shim);
            const worker = new OriginalWorker(wrapped.scriptUrl, wrapped.options);
            worker.addEventListener("message", relay);
            return worker;
        };
        Worker.prototype = OriginalWorker.prototype;
    }
}

// A blob script is copied with the shim in front of it; any other script is
// loaded by a wrapper blob that installs the shim and then imports it.
function __wbg_wrap_worker_url(url, options, install) {
    const shim = "(" + install.toString() + ")(postMessage);" + __wbg_wrap_worker_url.toString() + ";";
    let scriptUrl = url;
    if (typeof url === "string" && !url.startsWith("blob:")) {
        scriptUrl = new URL(url, location.href).href;
    }
    let source;
    if (typeof scriptUrl === "string" && scriptUrl.startsWith("blob:")) {
        const xhr = new XMLHttpRequest();
        xhr.open("GET", scriptUrl, false);
        xhr.send();
        source = shim + xhr.responseText;
    } else if (options?.type === "module") {
        source = shim + "await import(" + JSON.stringify(String(scriptUrl)) + ");";
    } else {
        source = shim + "importScripts(" + JSON.stringify(String(scriptUrl)) + ");";
    }
    const blob = new Blob([source], { type: "application/javascript" });
    return { scriptUrl: URL.createObjectURL(blob), options };
}

const OriginalWorker = Worker;
Worker = function (url, options) {
    const wrapped = __wbg_wrap_worker_url(url, options, __wbg_install_worker_console_shim);
    const worker = new OriginalWorker(wrapped.scriptUrl, wrapped.options);
    worker.addEventListener("message", e => {
        if (Array.isArray(e.data) && typeof e.data[0] === "string" && e.data[0].startsWith("__wbgtest_")) {
            const element = document.getElementById(nocapture ? "output" : "console_output");
            for (const message of e.data[1]) {
                element.appendChild(document.createTextNode(String(message) + "\n"));
            }
            e.stopImmediatePropagation();
        }
    });
    return worker;
};
Worker.prototype = OriginalWorker.prototype;

document.getElementById("output").textContent = "Loading Wasm module...\n";

const wasm = await init("./wasm-bindgen-test_bg.wasm");
const cx = new Context(false);
window.on_console_debug = __wbgtest_console_debug;
window.on_console_log = __wbgtest_console_log;
window.on_console_info = __wbgtest_console_info;
window.on_console_warn = __wbgtest_console_warn;
window.on_console_error = __wbgtest_console_error;
cx.include_ignored(__INCLUDE_IGNORED__);
cx.filtered_count(__FILTERED__);
window.__kithara_tests_ready = true;
await window.__kithara_user_activation;
await cx.run(__TESTS__.map(name => wasm[name]));
