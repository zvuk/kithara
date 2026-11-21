import {
    WasmBindgenTestContext as Context,
    __wbgtest_console_debug,
    __wbgtest_console_log,
    __wbgtest_console_info,
    __wbgtest_console_warn,
    __wbgtest_console_error,
    default as init,
} from "./wasm-bindgen-test.js";

const nocapture = __NOCAPTURE__;

self.__wbg_test_invoke = f => f();
self.__wbg_test_output_writeln = function (...args) {
    self.postMessage(["__wbgtest_output_append", args.map(String).join(" ") + "\n"]);
};

for (const method of ["debug", "log", "info", "warn", "error"]) {
    const on_method = `on_console_${method}`;
    self.console[method] = function (...args) {
        if (nocapture) {
            self.__wbg_test_output_writeln(...args);
        }
        if (self[on_method]) {
            self[on_method](args);
        }
    };
}

self.onmessage = async function (e) {
    const wasm = await init("./wasm-bindgen-test_bg.wasm");
    const cx = new Context(false);
    self.on_console_debug = __wbgtest_console_debug;
    self.on_console_log = __wbgtest_console_log;
    self.on_console_info = __wbgtest_console_info;
    self.on_console_warn = __wbgtest_console_warn;
    self.on_console_error = __wbgtest_console_error;
    cx.include_ignored(__INCLUDE_IGNORED__);
    cx.filtered_count(__FILTERED__);
    await cx.run(e.data.map(name => wasm[name]));
};
