use kithara::platform::{
    CancelToken,
    logging::{install_panic_hook, log_error},
    thread::set_wasm_shim_name,
    tokio::task,
};

/// Same `__heap_end` definition as `kithara-ffi`'s web bindings: an empty
/// `[__heap_base, __heap_end)` keeps dlmalloc off wasm-bindgen's thread-bootstrap block.
mod heap {
    #[used]
    #[unsafe(export_name = "__heap_end")]
    static HEAP_END: u8 = 0;
}

pub(super) fn main(shutdown: CancelToken) {
    install_panic_hook();
    set_wasm_shim_name(env!("CARGO_PKG_NAME"));
    drop(task::spawn(async move {
        if let Err(error) = kithara_app::web::run(shutdown).await {
            log_error(&format!("kithara failed: {error}"));
        }
    }));
}
