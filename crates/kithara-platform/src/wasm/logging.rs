use std::{panic, sync::Once};

use js_sys::JsString;
use web_sys::console;

/// `String.fromCharCode` takes one argument per code unit.
const CODE_UNITS_PER_CALL: usize = 1024;

/// Build a JS string from the code units of `msg`, which every scope can read.
fn js_string(msg: &str) -> JsString {
    let mut units = msg.encode_utf16();
    let mut line = JsString::from_char_code(&[]);
    let mut chunk = [0u16; CODE_UNITS_PER_CALL];

    loop {
        let mut filled = 0;
        for (slot, unit) in chunk.iter_mut().zip(units.by_ref()) {
            *slot = unit;
            filled += 1;
        }
        if filled == 0 {
            return line;
        }
        line = line.concat(&JsString::from_char_code(&chunk[..filled]));
    }
}

/// Emit an error-level diagnostic line through the platform-appropriate sink.
///
/// On native this routes through `tracing`. On wasm it writes to the browser
/// `console` directly: the global `tracing` subscriber marks its spans through
/// `performance`, which `AudioWorkletGlobalScope` leaves undefined.
pub fn log_error(msg: &str) {
    console::error_1(&js_string(msg));
}

/// Report a panic through [`log_error`], on whichever thread panicked.
///
/// `set_hook` writes one process-wide slot, so a binary's entry point owns this
/// call. `-C panic=immediate-abort` traps instead and reaches no hook.
pub fn install_panic_hook() {
    static INSTALLED: Once = Once::new();

    INSTALLED.call_once(|| {
        panic::set_hook(Box::new(|info| {
            let payload = info
                .payload_as_str()
                .unwrap_or("panic payload is not a string");
            match info.location() {
                Some(at) => log_error(&format!("panicked at {at}: {payload}")),
                None => log_error(&format!("panicked: {payload}")),
            }
        }));
    });
}
