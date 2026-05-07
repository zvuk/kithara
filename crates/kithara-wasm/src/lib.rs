// WASM HLS player library entry point.
//
// `kithara-wasm` is the browser/wasm platform shim (parallel to
// `kithara-platform` for native targets). All wasm-only code lives under
// the single cfg-gated `mod wasm;` below — see crate README for details.

#[cfg(target_arch = "wasm32")]
mod wasm;

#[cfg(target_arch = "wasm32")]
pub use wasm::bindings::build_info;
