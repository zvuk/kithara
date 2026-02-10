# WebAssembly Support Plan

Target: `wasm32-unknown-unknown` (browser).

## Current State Analysis

### Dependency Compatibility Matrix

| Dependency | WASM Status | Action |
|---|---|---|
| **rayon** | Via `wasm-bindgen-rayon` (nightly, SharedArrayBuffer) | Pool threads → Web Workers. Already documented in `pool.rs` |
| **tokio** (runtime) | Not supported on wasm32-unknown-unknown | Replace runtime; keep sync primitives |
| **tokio** (sync: Notify, broadcast, oneshot) | Works (pure Rust, no runtime needed) | Keep as-is |
| **reqwest** | Built-in WASM support (auto-switches to `fetch()` API) | Conditional features in Cargo.toml |
| **symphonia** | Pure Rust, works on WASM | Disable `opt-simd` on wasm (verify) |
| **mmap-io** | No mmap on WASM | Gate out; `MemResource` already exists |
| **parking_lot** | Partial (degraded mode on stable, full on nightly+atomics) | Keep — works inside Web Workers |
| **kanal** | Not compatible (custom mutex, thread parking) | Replace with `flume` on wasm |
| **crossbeam-queue** | Yes (lock-free, no_std+alloc) | Keep |
| **dashmap** | Partial (compiles, but `getrandom` chain issue) | Gate; use `HashMap` on wasm where concurrent access isn't needed |
| **rangemap** | Yes (pure data structure) | Keep |
| **lru** | Yes (no_std) | Keep |
| **hls_m3u8** | Yes (pure parser) | Keep |
| **bincode** | Yes | Keep |
| **ringbuf** | Yes (no_std) | Keep |
| **tempfile** | No filesystem on WASM | Gate out (test-only in prod code) |

### Crate-by-Crate Blockers

#### kithara-storage (MEDIUM)
- **Blocker**: `MmapDriver` uses `mmap-io` → no mmap on WASM.
- **Solution**: `MemDriver` already exists (`memory.rs`). On WASM, only `MemResource` is available.
- **Blocker**: `driver.rs` uses `parking_lot::Condvar` for `wait_range()` blocking.
- **Solution**: `Condvar` works inside Web Workers (rayon pools). The reader thread runs
  on rayon pool, so blocking is allowed. On main thread — never called directly.
- **Action**: `#[cfg(not(target_arch = "wasm32"))]` gate on `mmap.rs` and `mmap-io` dependency.

#### kithara-stream (HIGH — core architecture)
- **Blocker**: `backend.rs:56-58` — `tokio::runtime::Handle::block_on()` inside rayon thread.
  ```rust
  let handle = tokio::runtime::Handle::current();
  pool.spawn(move || {
      handle.block_on(Self::run_downloader(downloader, task_cancel));
  });
  ```
  Tokio runtime doesn't exist on WASM. This is the single most critical blocker.
- **Solution**: On WASM, run the downloader as a plain async task via
  `wasm_bindgen_futures::spawn_local()` instead of blocking on a rayon thread.
  The downloader is inherently async — wrapping it in `block_on` on rayon was an
  optimization for native (avoid occupying tokio threads with long-running tasks).
  On WASM, the JS event loop handles async natively.
- **Blocker**: `pool.rs` — `spawn_async()` uses `tokio::sync::oneshot` (fine, no runtime needed)
  but the calling pattern assumes a tokio runtime exists.
- **Action**: Platform-split in `Backend::new()`:
  - Native: keep current `handle.block_on()` on rayon.
  - WASM: `wasm_bindgen_futures::spawn_local(run_downloader(...))`.

#### kithara-net (LOW)
- **Blocker**: `reqwest` features `["rustls", "stream"]` — `rustls` is irrelevant on WASM
  (browser handles TLS via `fetch()`).
- **Solution**: Conditional dependency:
  ```toml
  [target.'cfg(not(target_arch = "wasm32"))'.dependencies]
  reqwest = { version = "0.13.2", default-features = false, features = ["rustls", "stream"] }

  [target.'cfg(target_arch = "wasm32")'.dependencies]
  reqwest = { version = "0.13.2", default-features = false, features = ["stream"] }
  ```
- **Blocker**: `client.rs` uses `pool_max_idle_per_host` — not available on WASM.
- **Action**: `#[cfg]` gate on client builder options.

#### kithara-decode (LOW)
- Symphonia is pure Rust — works on WASM.
- `opt-simd` feature may need verification (WASM SIMD is supported in browsers but
  Rust intrinsics mapping needs testing).
- Apple/Android hardware decoder modules are already feature-gated — no impact.
- **Action**: Verify `symphonia` compiles with `--target wasm32-unknown-unknown`.
  Consider disabling `opt-simd` on wasm if it pulls problematic deps.

#### kithara-audio (MEDIUM)
- **Blocker**: `worker.rs` uses `std::thread::sleep()` for backpressure (lines 70, 121).
  This runs on a rayon pool thread → Web Worker on WASM → blocking is allowed.
- **Blocker**: Uses `kanal` channels for PCM pipeline. kanal doesn't support WASM.
- **Solution**: Replace `kanal` with `flume` (explicit wasm support, similar API:
  `bounded()`, `try_send()`, `try_recv()`, `recv()`).
- **Blocker**: `audio.rs` uses `tokio::spawn` for event forwarding (line 389).
- **Solution**: On WASM, replace with `wasm_bindgen_futures::spawn_local()`.
- **Action**: Abstract channel type behind a cfg gate or use `flume` everywhere.

#### kithara-hls (MEDIUM)
- **Blocker**: `source.rs` uses `parking_lot::Condvar` for segment availability sync.
  Same pattern as storage — reader blocks on condvar waiting for downloader to write.
  This runs on a rayon pool thread, so Web Worker blocking is fine.
- **Blocker**: `DashMap` used in `fetch.rs` — partial WASM support, `getrandom` issue.
- **Solution**: Enable `getrandom/js` feature when targeting WASM. Or replace with
  `HashMap` + `Mutex` on wasm.
- **Action**: Add `getrandom = { version = "0.3", features = ["wasm_js"] }` for wasm target.

#### kithara-assets (LOW-MEDIUM)
- `DiskAssetStore` uses filesystem — gate out on WASM.
- `MemAssetStore` / `MemStore` already exist for ephemeral in-memory storage.
- On WASM, `AssetStore` type alias should point to `MemAssetStore`.
- **Action**: `#[cfg]` gate on disk-related code.

#### kithara-bufpool (LOW)
- Uses `parking_lot::Mutex` — works in Web Workers.
- Pure data structure, no I/O.
- **Action**: Should work as-is.

#### kithara-abr (NONE)
- Pure algorithm, only depends on `tracing`.
- **Action**: Works as-is.

#### kithara-file (LOW)
- Depends on `kithara-stream`, `kithara-net`, `kithara-storage`.
- No additional blockers beyond what's listed above.

#### kithara (facade) (LOW)
- `rodio` feature already optional and gated.
- Wire up wasm feature flag to propagate to sub-crates.

## Architecture: Threading Model on WASM

```
                    NATIVE                          WASM (browser)
                    ──────                          ──────────────
Async runtime:     tokio (multi-thread)            JS event loop
Downloader:        rayon thread + block_on(async)  spawn_local(async) on main thread
                                                    OR dedicated Web Worker
Audio worker:      rayon thread (blocking loop)    Web Worker (via wasm-bindgen-rayon)
Decoder:           sync on rayon thread            sync on Web Worker
Storage:           mmap + condvar                  MemResource + condvar (in Worker)
Channels:          kanal (sync MPMC)               flume (sync+async, wasm-compatible)
HTTP:              reqwest + rustls                reqwest + fetch() API
```

### Why rayon works

`wasm-bindgen-rayon` maps rayon pool threads to Web Workers. Inside a Web Worker:
- `std::thread::sleep()` works (via `Atomics.wait`).
- `parking_lot::Mutex`/`Condvar` work (atomics available with `+atomics` feature).
- Blocking synchronous code runs without blocking the main browser thread.

Requirements:
- Nightly toolchain with `rust-src` component.
- Build stdlib with: `-C target-feature=+atomics,+bulk-memory,+mutable-globals`.
- Hosting page needs Cross-Origin Isolation headers:
  `Cross-Origin-Opener-Policy: same-origin`
  `Cross-Origin-Embedder-Policy: require-corp`
- Initialize pool from JS: `await initThreadPool(navigator.hardwareConcurrency)`.

### The block_on problem

The only production `block_on` call is in `backend.rs:56-58`. On WASM:

**Option A** — Run downloader on main thread as async task:
```rust
#[cfg(target_arch = "wasm32")]
wasm_bindgen_futures::spawn_local(Self::run_downloader(downloader, task_cancel));

#[cfg(not(target_arch = "wasm32"))]
{
    let handle = tokio::runtime::Handle::current();
    pool.spawn(move || {
        handle.block_on(Self::run_downloader(downloader, task_cancel));
    });
}
```

Pros: Simple, no additional executor. `reqwest` on WASM uses fetch() which integrates
with the JS event loop natively.

Cons: Downloader runs on main thread. But it's async and yields regularly, so UI
won't block. All heavy work (decode, probe) still runs on rayon Web Workers.

**Option B** — Run downloader on a dedicated Web Worker with a local executor:
More complex. Requires either `futures::executor::LocalPool` inside a Worker or a
custom `block_on` that pumps the JS microtask queue via `Atomics.wait` + `Atomics.notify`.

**Recommendation**: Option A. The downloader is I/O-bound (network fetches), not
CPU-bound. Running it on the main thread as an async task is natural for the browser.
All CPU-bound work (decode, probe, audio effects) stays on rayon Web Workers.

## Implementation Plan

### Phase 1: Feature flags and conditional compilation

1. Add `wasm` feature to workspace and all crates (or use `#[cfg(target_arch = "wasm32")]`).
   Prefer `cfg(target_arch)` — no feature flag needed, automatic per target.

2. Add workspace dependencies for WASM:
   ```toml
   [workspace.dependencies]
   wasm-bindgen = "0.2"
   wasm-bindgen-futures = "0.4"
   wasm-bindgen-rayon = "1.3"
   js-sys = "0.3"
   web-sys = { version = "0.3", features = ["console"] }
   getrandom = { version = "0.3", features = ["wasm_js"] }
   flume = "0.11"
   ```

3. Split platform-specific dependencies in each crate's `Cargo.toml`:
   ```toml
   [target.'cfg(not(target_arch = "wasm32"))'.dependencies]
   tokio = { workspace = true }  # full features
   mmap-io = { workspace = true }

   [target.'cfg(target_arch = "wasm32")'.dependencies]
   wasm-bindgen-futures = { workspace = true }
   getrandom = { workspace = true }
   ```

### Phase 2: kithara-storage

1. Gate `mmap.rs` and `MmapDriver` behind `#[cfg(not(target_arch = "wasm32"))]`.
2. `MemResource` is the only storage backend on WASM.
3. No changes to `driver.rs` — `Condvar` works inside Web Workers.

### Phase 3: kithara-stream (critical path)

1. Platform-split `Backend::new()`:
   - Native: existing `handle.block_on()` on rayon.
   - WASM: `wasm_bindgen_futures::spawn_local()`.

2. `ThreadPool` — verify `wasm-bindgen-rayon` integration. Add initialization export:
   ```rust
   #[cfg(target_arch = "wasm32")]
   pub use wasm_bindgen_rayon::init_thread_pool;
   ```

3. `pool.rs` `spawn_async()` — replace `tokio::sync::oneshot` with a platform-agnostic
   oneshot or keep it (tokio sync primitives don't need a runtime).

### Phase 4: kithara-net

1. Conditional reqwest features (drop `rustls` on wasm).
2. Gate `pool_max_idle_per_host` and other native-only client options.
3. Timeouts: `ClientBuilder::timeout()` doesn't work on WASM. Use
   `AbortController` via `web-sys` or skip client-level timeout on WASM.

### Phase 5: kithara-audio

1. Replace `kanal` with `flume` (or make it conditional).
   `flume` API is nearly identical: `flume::bounded()`, `.try_send()`, `.try_recv()`.
   `flume` has explicit WASM support.
2. Gate `tokio::spawn` for event forwarding:
   - Native: `tokio::spawn`.
   - WASM: `wasm_bindgen_futures::spawn_local`.

### Phase 6: kithara-hls

1. `DashMap` — add `getrandom` with `wasm_js` feature to resolve `HashMap` seeding.
2. Verify `hls_m3u8` compiles for wasm32.
3. `source.rs` Condvar — works in Web Workers, no change needed.

### Phase 7: kithara-assets

1. Gate `DiskAssetStore` behind `#[cfg(not(target_arch = "wasm32"))]`.
2. Redefine `AssetStore` type alias for WASM:
   ```rust
   #[cfg(target_arch = "wasm32")]
   pub type AssetStore = MemAssetStore;
   ```

### Phase 8: Build tooling

1. Add `.cargo/config.toml` with wasm build profile:
   ```toml
   [target.wasm32-unknown-unknown]
   rustflags = ["-C", "target-feature=+atomics,+bulk-memory,+mutable-globals"]
   ```

2. Add `rust-toolchain.toml` entry or build script for nightly wasm builds:
   ```toml
   # When building for wasm, use:
   # rustup target add wasm32-unknown-unknown
   # rustup component add rust-src
   # cargo +nightly build --target wasm32-unknown-unknown -Z build-std=panic_abort,std
   ```

3. Add CI job: `cargo check --target wasm32-unknown-unknown` (compile gate).

4. Add `wasm-pack test --headless --chrome` for runtime tests.

### Phase 9: Tokio feature split

The root `Cargo.toml` currently uses `tokio = { features = ["full"] }`. For WASM,
tokio is not used as a runtime — only its sync primitives are needed.

```toml
[workspace.dependencies]
# Native: full tokio
tokio = { version = "1.49.0", features = ["full"] }

# For wasm targets, crates should only use tokio::sync primitives.
# tokio-util's CancellationToken is also pure sync.
```

Crates that only need `tokio::sync` (Notify, broadcast, oneshot) and
`tokio_util::sync::CancellationToken` should work on WASM with:
```toml
[target.'cfg(target_arch = "wasm32")'.dependencies]
tokio = { version = "1.49.0", default-features = false, features = ["sync", "macros"] }
tokio-util = { version = "0.7.18", default-features = false }
```

## Risk Assessment

| Risk | Severity | Mitigation |
|---|---|---|
| Nightly-only (atomics, build-std) | Medium | Required by wasm-bindgen-rayon. Track stabilization of wasm atomics. |
| Cross-Origin Isolation headers | Low | Standard requirement. Document for consumers. |
| SharedArrayBuffer availability | Low | Supported in all modern browsers since 2021. |
| Symphonia WASM perf | Medium | Pure Rust is slower than SIMD. Test decode perf. Consider WebCodecs API as future alternative. |
| kanal → flume migration | Low | APIs are similar. Benchmark to verify no regression on native. |
| tokio::select! on WASM | Medium | The macro works without runtime, but `cancel.cancelled()` needs verification. |

## Files Requiring Changes (Production Code Only)

```
CRITICAL (Phase 3):
  crates/kithara-stream/src/backend.rs     — block_on → spawn_local on wasm
  crates/kithara-stream/src/pool.rs        — wasm-bindgen-rayon re-export

HIGH (Phase 1-2):
  Cargo.toml                               — workspace deps (wasm crates, flume)
  crates/kithara-storage/Cargo.toml        — gate mmap-io
  crates/kithara-storage/src/lib.rs        — gate MmapResource re-export
  crates/kithara-storage/src/mmap.rs       — gate entire module
  crates/kithara-net/Cargo.toml            — conditional reqwest features
  crates/kithara-net/src/client.rs         — gate native-only builder opts

MEDIUM (Phase 4-7):
  crates/kithara-audio/Cargo.toml          — add flume, gate kanal
  crates/kithara-audio/src/pipeline/audio.rs — flume channels, spawn gate
  crates/kithara-audio/src/pipeline/worker.rs — flume channel types
  crates/kithara-hls/Cargo.toml            — getrandom wasm feature
  crates/kithara-assets/src/lib.rs         — gate DiskAssetStore type alias
  crates/kithara-assets/src/store.rs       — gate disk store module

LOW (Phase 8-9):
  .cargo/config.toml                       — wasm rustflags
  Each crate's Cargo.toml                  — tokio feature split for wasm
```

## What Stays Unchanged

- `kithara-abr` — pure algorithm, no changes.
- `kithara-bufpool` — parking_lot works in Web Workers, no changes.
- `kithara-decode` (core) — Symphonia is pure Rust, decoder logic unchanged.
- `kithara-hls/src/source.rs` — Condvar works in Web Workers.
- `kithara-storage/src/driver.rs` — Condvar works in Web Workers.
- `kithara-audio/src/pipeline/worker.rs` — `std::thread::sleep` works in Web Workers.
- All test code — tests run on native only.
- `rodio` — already gated, desktop-only (per user requirement).

## Open Questions

1. **kanal → flume**: Benchmark on native to confirm no perf regression in hot audio path.
   If flume is measurably slower, keep kanal on native and use flume only on wasm via cfg.

2. **Symphonia SIMD on WASM**: Does `opt-simd` compile for wasm32? If not, disable
   for wasm target. WebAssembly SIMD is supported but Rust → WASM SIMD mapping may
   require `std::arch::wasm32` intrinsics rather than x86/ARM intrinsics.

3. **tokio::select! without runtime**: Verify that `tokio::select!` macro works when
   only `tokio = { features = ["sync", "macros"] }` is enabled (no `rt`). The macro
   itself doesn't need a runtime, but some futures within it might.

4. **Audio output on WASM**: Outside kithara's scope (kithara is decode + streaming,
   not playback), but consumers will need Web Audio API integration.
   Consider providing a minimal `web-sys` AudioWorklet example.
