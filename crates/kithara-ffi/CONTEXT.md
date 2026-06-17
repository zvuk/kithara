# kithara-ffi — Context

Detailed contracts and invariants for the kithara-ffi crate; the README is the overview.

## Consumers

- On Android, `src/android_test.rs` is gated on `target_os = "android"` + `feature = "test"` and is one of two consumers of `kithara-play` from this crate (the other being `src/web/*` on `target_arch = "wasm32"`).

## Web target

`src/lib.rs` is the single structural boundary where `target_arch = "wasm32"` lives: `pub mod web;` is gated; everything under `src/web/` is unconditionally wasm-only. Likewise, `pub(crate) mod android;` is the only `target_os` gate in this file. The `arch.no-target-os-outside-platform` lint exempts `src/lib.rs` for exactly that reason; everywhere else inside the crate must stay free of inline `cfg(target_*)` gates.

`src/web/` uses `#[wasm_bindgen]` directly for the JS-facing surface. The browser surface is the cross-platform [`AudioPlayer`](src/player/facade.rs) facade with a `#[wasm_bindgen] impl` in [`src/web/surface.rs`](src/web/surface.rs): JS constructs one `new AudioPlayer()` and drives the queue through `append` / `insert` / `selectItem`, transport through `play` / `pause` / `seek`, and receives structured events through `setObserver` / `setItemObserver`. The worker (`src/web/worker.rs`) owns the `Queue`; the main-thread `WasmInner` forwards commands and answers infallible getters from a local cache. Generated TypeScript definitions ship with the wasm-bindgen output.

## Build flow internals

- `cargo xtask wasm postbuild` — post-build patches for COEP/COOP, polyfills, and the `checkRuntime()` helper appended to `kithara-ffi.js`.
