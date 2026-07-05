# kithara-ffi — Context

Detailed contracts and invariants for the kithara-ffi crate; the README is the overview.

## Consumers

- On Android, `src/native/android_test.rs` is gated in `src/native/mod.rs` on
  `target_os = "android"` + `feature = "test"` and imports the `kithara`
  facade. Direct `kithara-play` consumers in this crate are `src/core/types.rs`
  and the wasm files under `src/web/`.

## Web target

`src/lib.rs` gates the high-level target split: `mod native` for non-wasm and
`pub mod web` for wasm. Android-specific gates live in `src/native/mod.rs`
(`mod android`, `mod android_test`). The `arch.no-target-os-outside-platform`
lint exempts `src/lib.rs` for the structural target-arch split; platform-specific
submodules own their narrower target gates.

`src/web/` uses `#[wasm_bindgen]` directly for the JS-facing surface. The browser surface is the cross-platform [`AudioPlayer`](src/player/facade.rs) facade with a `#[wasm_bindgen] impl` in [`src/web/surface.rs`](src/web/surface.rs): JS constructs one `new AudioPlayer()` and drives the queue through `append` / `insert` / `selectItem`, transport through `play` / `pause` / `seek`, and receives structured events through `setObserver` / `setItemObserver`. The worker (`src/web/worker.rs`) owns the `Queue`; the main-thread `WasmInner` forwards commands and answers infallible getters from a local cache. Generated TypeScript definitions ship with the wasm-bindgen output.

## Build flow internals

- `cargo xtask wasm postbuild` — post-build patches for COEP/COOP, polyfills, and the `checkRuntime()` helper appended to `kithara-ffi.js`.
