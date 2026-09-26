<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)

</div>

# kithara-ffi

Cross-platform FFI adapter for the kithara audio player. Not published — consumed by Apple (Swift via UniFFI), Android (Kotlin via UniFFI / JNI), and browser (wasm-bindgen) build flows.

## Usage

See the workspace tooling for end-to-end builds:

- `just platform apple xcframework` — builds the Apple XCFramework (release).
- `just platform android aar` — builds Android AARs (release).
- `just platform wasm build` — builds the browser demo via Trunk (output in `dist/`).
- `just tooling xtask wasm postbuild` — post-build patches for the wasm output.

Apple and Android builds use the `standard` feature set unless
`KITHARA_FFI_FEATURES` supplies a comma-separated replacement. Apple and Android
always include their required Signalsmith time-stretch backend; an empty value
removes only optional capabilities. WASM keeps its required `wasm` feature and
adds the selected features. For example:

```sh
KITHARA_FFI_FEATURES= just platform apple xcframework
KITHARA_FFI_FEATURES=standard,analysis just platform android aar
KITHARA_FFI_FEATURES=ui-iced just platform wasm build
```

The crate features are the source of truth for available capabilities. Platform
features select only their platform backend and do not implicitly enable
analysis or UI. UI backends keep their own target support constraints.

## Integration

- Exposes a stable, language-agnostic surface over `kithara-play` so platform shims (`kithara/apple`, `kithara/android`, and the browser demo) can talk to the engine without depending on internal Rust types.
- Owns the UniFFI definitions used by `just platform apple xcframework` and `just platform android aar`.
- Exposes native Rust-owned `FfiAssetLayoutRegistry` and `FfiAssetStore` objects. The store snapshots protocol layouts, owns the cache root and runtime resources, and is injected through the single `FfiPlayerConfig.store` field.
- Owns the wasm-bindgen / Web Worker glue under [`src/web/`](src/web/) and the Trunk-driven demo (`index.html` + `Trunk.toml`).

Do not depend on this crate directly from application code — use the platform-specific shims (`kithara/apple`, `kithara/android`).

### Layout

- `src/` — UniFFI bindings + error mapping (native targets).
- `src/native/asset/` — Rust-owned native layout registry and shareable asset-store lifetime.
- `src/web/` — wasm-bindgen / Web Worker bindings (compiled only for `target_arch = "wasm32"`).
- `uniffi.toml` — UniFFI configuration consumed by the xtask build flows.
- `Trunk.toml` / `index.html` / `_headers` / `coi-serviceworker.js` - wasm demo app shell (used by `just platform wasm build` and selenium tests); the `kithara-app` web shell copies the same `_headers` and `coi-serviceworker.js`.

The browser surface is the cross-platform [`AudioPlayer`](src/player/facade.rs) facade with a `#[wasm_bindgen] impl` in [`src/web/surface.rs`](src/web/surface.rs).

Native callers create a registry, register file and HLS layout callbacks, then
create one `FfiAssetStore` from the outer cache root and a registry snapshot.
The same store object can be supplied to multiple player configurations. The
browser worker instead owns its in-memory `AssetStore`; the native UniFFI store
and registry are not part of the wasm API.

See [crate contracts](https://github.com/zvuk/kithara/wiki/kithara-ffi) for detailed contracts, invariants, and internals.
