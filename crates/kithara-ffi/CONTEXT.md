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

The native cache object graph is owned in Rust:

- `FfiAssetLayoutRegistry` is a shareable UniFFI object containing the Rust
  `AssetLayoutRegistry` behind a mutex. `register` replaces the layout for the
  file or HLS target; targets are independent and the latest registration for
  one target is the registry's current value.
- `FfiAssetStore::new(root, registry)` snapshots the registry and builds one
  `AssetStore`. `root = None` preserves the platform `StorageBackend` default;
  a supplied root selects that outer disk directory without changing paths
  inside an asset root.
- `FfiAssetStore` also owns the `Region` used by cache, network, decode, and
  playback plus a store-specific `CancelScope`. Dropping the last foreign
  `Arc<FfiAssetStore>` cancels that store subtree. Player cancellation is a
  separate scope and does not redefine the shared store lifetime.
- `FfiPlayerConfig.store: Arc<FfiAssetStore>` is the only asset/cache field on
  the player configuration. `NativeInner` retains that object, takes its
  `Region`, and clones its inner `AssetStore` handle into the queue and every
  resource. The same FFI store can therefore be shared by multiple players.

Registry mutation and store configuration are separate lifetimes. Registering
or replacing a layout after a store has been created does not alter that store;
only a later `FfiAssetStore::new` observes the new snapshot. A store retains the
foreign callbacks captured by its snapshot even if the registry or the
caller's original callback reference is released. Replacing a registry entry
does not release a callback still retained by an existing store.

Foreign `root` and `path` callbacks receive complete owned FFI values. `root`
is invoked once per asset-scope construction and `path` once per resource-key
construction. After a key is minted, cloning the scope and all acquire, open,
read, write, seek, state, availability, demand, and eviction operations stay in
Rust and do not cross the FFI callback boundary. Repeating scope or key
construction invokes the corresponding callback again.

Callbacks must be deterministic, fast, non-blocking, non-throwing, and safe on
background threads. Invalid output fails scope or key creation; it is neither
sanitized nor replaced with the default layout. A URL resource contains the
full URL, so custom delegates must preserve any required query identity without
writing query text, credentials, or other secrets into a path. The default
layout uses a bounded query fingerprint and ignores fragments.

An omitted exact target registration uses the documented `DefaultLayout`. This
is the normal layout default, not a compatibility fallback. Its disk mapping is
unchanged: direct-file bytes use `track/track.<ext>`, HLS URL resources mirror
authority and path below `track/` with a query fingerprint when needed, and the
named track analysis artifact uses `analysis/track.analysis`. Changing the
outer store root does not change those relative paths.

There is no `FfiCacheConfig`, `FfiCacheLayoutRegistration`, registration-list
translation, SDK-owned registry, or per-player store builder in the native
contract. Generated native bindings expose the Rust registry and store objects;
platform adapters only retain and forward those objects. Invalid custom output
does not fall back to `DefaultLayout`, and old cache configuration records are
not accepted through a compatibility path.

## Web target

`src/lib.rs` gates the high-level target split: `mod native` for non-wasm and
`pub mod web` for wasm. Android-specific gates live in `src/native/mod.rs`
(`mod android`, `mod android_test`). The `arch.no-target-os-outside-platform`
lint exempts `src/lib.rs` for the structural target-arch split; platform-specific
submodules own their narrower target gates.

`FfiAssetLayoutRegistry`, `FfiAssetStore`, and `FfiPlayerConfig` belong to the
native module and are not part of the wasm surface. The web worker constructs
one in-memory `AssetStore` and `Region` in its Rust-owned build state and uses
the default layout registry. Native foreign layout callbacks are not bridged
into JavaScript.

`src/web/` uses `#[wasm_bindgen]` directly for the JS-facing surface. The browser surface is the cross-platform [`AudioPlayer`](src/player/facade.rs) facade with a `#[wasm_bindgen] impl` in [`src/web/surface.rs`](src/web/surface.rs): JS constructs one `new AudioPlayer()` and drives the queue through `append` / `insert` / `selectItem`, transport through `play` / `pause` / `seek`, and receives structured events through `setObserver` / `setItemObserver`. The worker (`src/web/worker.rs`) owns the `Queue`; the main-thread `WasmInner` forwards commands and answers infallible getters from a local cache. Generated TypeScript definitions ship with the wasm-bindgen output.

Wasm builds use the web-audio backend and no native stretch backend. The shared
`StretchControls` rate is retained as control state, but PCM speed is pinned to
1.0 until a wasm-capable stretch backend exists.

## Transport event migration

`TransportEvent` and `SyncEvent` own all new transport and track-sync facts.
Their FFI variants use `Transport*` and `Sync*` names on UniFFI and WASM
surfaces. The existing `DjBpmDetected`, `DjKeylockChanged`, and
`DjStretchBackendChanged` wire tags remain stable only for their existing
legacy events; new transport or sync facts must not use `Dj*` identifiers.

## Build flow internals

- `cargo xtask wasm postbuild` — post-build patches for COEP/COOP, polyfills, and the `checkRuntime()` helper appended to `kithara-ffi.js`.
