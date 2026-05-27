<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-ffi

Cross-platform FFI adapter for the kithara audio player. Not published — consumed by Apple (Swift via UniFFI), Android (Kotlin via UniFFI / JNI), and browser (wasm-bindgen) build flows.

## Role

- Exposes a stable, language-agnostic surface over `kithara-play` so platform shims (`kithara/apple`, `kithara/android`, and the browser demo) can talk to the engine without depending on internal Rust types.
- Owns the UniFFI definitions used by `cargo xtask apple xcframework` and `cargo xtask android aar`.
- Owns the wasm-bindgen / Web Worker glue under [`src/web/`](src/web/) and the Trunk-driven demo (`index.html` + `Trunk.toml`).
- On Android, `src/android_test.rs` is gated on `target_os = "android"` + `feature = "test"` and is one of two consumers of `kithara-play` from this crate (the other being `src/web/*` on `target_arch = "wasm32"`).

## Layout

- `src/` — UniFFI bindings + error mapping (native targets).
- `src/web/` — wasm-bindgen / Web Worker bindings (compiled only for `target_arch = "wasm32"`).
- `uniffi.toml` — UniFFI configuration consumed by the xtask build flows.
- `Trunk.toml` / `index.html` / `_headers` / `coi-serviceworker.js` — wasm demo app shell (used by `cargo xtask wasm build` and selenium tests).

## Web target

`src/lib.rs` is the single structural boundary where `target_arch = "wasm32"` lives: `pub mod web;` is gated; everything under `src/web/` is unconditionally wasm-only. Likewise, `pub(crate) mod android;` is the only `target_os` gate in this file. The `arch.no-target-os-outside-platform` lint exempts `src/lib.rs` for exactly that reason; everywhere else inside the crate must stay free of inline `cfg(target_*)` gates.

`src/web/` uses `#[wasm_bindgen]` directly for the JS-facing surface (the Player singleton is exposed via free `player_*` functions; the xtask wasm postbuild step synthesises a JS `Player` class wrapper around them). Generated TypeScript definitions ship with the wasm-bindgen output.

## Build flows

See the workspace tooling for end-to-end builds:

- `just apple xcframework` — builds the Apple XCFramework (release).
- `just android aar` — builds Android AARs (release).
- `cargo xtask wasm build` — builds the browser demo via Trunk (output in `dist/`).
- `cargo xtask wasm postbuild` — post-build patches for COEP/COOP, polyfills, and the JS `Player` class wrapper.

This crate is `publish = false`. Do not depend on it directly from application code — use the platform-specific shims (`kithara/apple`, `kithara/android`).
