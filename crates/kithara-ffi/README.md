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
- Owns the UniFFI definitions used by `cargo xtask apple build` and `cargo xtask android aar`.
- Exposes native Rust-owned `FfiAssetLayoutRegistry` and `FfiAssetStore` objects. The store snapshots protocol layouts, owns the cache root and runtime resources, and is injected through the single `FfiPlayerConfig.store` field.
- Owns the wasm-bindgen / Web Worker glue under [`src/web/`](src/web/) and the Trunk-driven demo (`index.html` + `Trunk.toml`).

## Layout

- `src/` — UniFFI bindings + error mapping (native targets).
- `src/native/asset/` — Rust-owned native layout registry and shareable asset-store lifetime.
- `src/web/` — wasm-bindgen / Web Worker bindings (compiled only for `target_arch = "wasm32"`).
- `uniffi.toml` — UniFFI configuration consumed by the xtask build flows.
- `Trunk.toml` / `index.html` / `_headers` / `coi-serviceworker.js` — wasm demo app shell (used by `cargo xtask wasm build` and selenium tests).

The browser surface is the cross-platform [`AudioPlayer`](src/player/facade.rs) facade with a `#[wasm_bindgen] impl` in [`src/web/surface.rs`](src/web/surface.rs).

Native callers create a registry, register file and HLS layout callbacks, then
create one `FfiAssetStore` from the outer cache root and a registry snapshot.
The same store object can be supplied to multiple player configurations. The
browser worker instead owns its in-memory `AssetStore`; the native UniFFI store
and registry are not part of the wasm API.

## Build flows

See the workspace tooling for end-to-end builds:

- `just apple xcframework` — builds the Apple XCFramework (release).
- `just android aar` — builds Android AARs (release).
- `cargo xtask wasm build` — builds the browser demo via Trunk (output in `dist/`).
- `cargo xtask wasm postbuild` — post-build patches for the wasm output.

This crate is `publish = false`. Do not depend on it directly from application code — use the platform-specific shims (`kithara/apple`, `kithara/android`).

See [CONTEXT.md](CONTEXT.md) for detailed contracts, invariants, and internals.
