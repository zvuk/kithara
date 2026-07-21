#![forbid(unsafe_code)]
#![cfg(target_arch = "wasm32")]

mod browser_runner_smoke;
#[path = "kithara_play/track_binding.rs"]
mod track_binding;
#[path = "kithara_play/wasm_transport.rs"]
mod wasm_transport;
