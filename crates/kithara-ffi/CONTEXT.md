# kithara-ffi — Context

Detailed contracts and invariants for the kithara-ffi crate; the README is the overview.

## Consumers

- On Android, `src/native/android_test.rs` is gated in `src/native/mod.rs` on
  `target_os = "android"` + `feature = "test"` and imports the `kithara`
  facade. Direct `kithara-play` consumers in this crate are `src/core/types.rs`
  and the wasm files under `src/web/`.

## Device feature sets

`xtask apple` builds device frameworks with
`uniffi,apple,dev,stretch-signalsmith`. The crate-local `apple` feature forwards
`kithara/apple-fused-src`, so Apple AudioToolbox decodes directly to the host
rate through decoder-embedded resampler placement. That set intentionally does
not enable `resample-rubato`, `analysis-beat`, or `analysis-waveform`.

`xtask android` builds release JNI libraries with
`uniffi,android,stretch-signalsmith`. The facade `android` feature keeps the
fixed-ratio rubato stage and beat analysis enabled; Android does not use the
Apple fused SRC path.

## Cache ownership and layouts

`FfiPlayerConfig.cache: FfiCacheConfig` is the only native cache
configuration input. Player construction creates one `AssetStore`; cheap
clones of that same `Arc`-backed handle are passed to every file, HLS, playback,
and analysis resource owned by the player. `cache_dir` selects the outer disk
root, while `layouts` independently controls paths within each asset root.

Each `FfiCacheLayoutRegistration` targets either the file or HLS protocol
marker. Registrations are applied in order, so the last registration for a
target wins. An unregistered target uses `DefaultLayout`; there is no parallel
options type, per-item store builder, or singular fallback layout.

Foreign `root` and `path` callbacks receive complete owned FFI values. `root`
is invoked once per store scope and `path` once per resource key derivation.
After a key is minted, cloning the scope and all acquire, open, read, write,
seek, state, availability, demand, and eviction operations stay in Rust and do
not cross the FFI callback boundary. Repeating scope or key derivation invokes
the corresponding callback again.

Callbacks must be deterministic, fast, non-blocking, non-throwing, and safe on
background threads. Invalid output fails scope or key creation; it is neither
sanitized nor replaced with the default layout. A URL resource contains the
full URL, so custom delegates must preserve any required query identity without
writing query text, credentials, or other secrets into a path. The default
layout uses a bounded query fingerprint and ignores fragments.

The Apple wrapper exposes this contract as
`KitharaPlayer.Config(cacheDir:layouts:)` and `CacheLayoutRegistry`. The
registry is converted to an immutable FFI registration list when the player is
created.

## Web target

`src/lib.rs` gates the high-level target split: `mod native` for non-wasm and
`pub mod web` for wasm. Android-specific gates live in `src/native/mod.rs`
(`mod android`, `mod android_test`). The `arch.no-target-os-outside-platform`
lint exempts `src/lib.rs` for the structural target-arch split; platform-specific
submodules own their narrower target gates.

`src/web/` uses `#[wasm_bindgen]` directly for the JS-facing surface. The browser surface is the cross-platform [`AudioPlayer`](src/player/facade.rs) facade with a `#[wasm_bindgen] impl` in [`src/web/surface.rs`](src/web/surface.rs): JS constructs one `new AudioPlayer()` and drives the queue through `append` / `insert` / `selectItem`, transport through `play` / `pause` / `seek`, and receives structured events through `setObserver` / `setItemObserver`. The worker (`src/web/worker.rs`) owns the `Queue`; the main-thread `WasmInner` forwards commands and answers infallible getters from a local cache. Generated TypeScript definitions ship with the wasm-bindgen output.

Wasm builds use the web-audio backend and no native stretch backend. The shared
`StretchControls` rate is retained as control state, but PCM speed is pinned to
1.0 until a wasm-capable stretch backend exists.

## Build flow internals

- `cargo xtask wasm postbuild` — post-build patches for COEP/COOP, polyfills, and the `checkRuntime()` helper appended to `kithara-ffi.js`.
