# WASM Examples Design

## Goal

Create example applications that compile to WebAssembly and run in the browser,
demonstrating kithara's capabilities. Phased approach: ABR demo → decode demo → full HLS player.

## Organization

**New crate:** `crates/kithara-wasm` — thin bin crate with `#[wasm_bindgen]` entry points.
All logic stays in existing crates. Browser-specific adaptations use `cfg(target_arch = "wasm32")`.

**Build tool:** `wasm-server-runner` (Phases 1-2), Trunk (Phase 3 for COOP/COEP headers).

**Run command:**
```bash
cargo run -p kithara-wasm --target wasm32-unknown-unknown
```

## WASM Compatibility Summary

| Dependency | WASM Support | Notes |
|---|---|---|
| symphonia | Yes (pure Rust) | Used by Ruffle. `opt-simd` may need cfg-gate |
| reqwest | First-class | Auto-switches to fetch() on wasm32 |
| rodio/cpal | Yes | `wasm-bindgen` feature, ScriptProcessorNode |
| tokio | Partial | `sync`, `macros`, `rt` only. `time` panics on wasm32 |
| rayon | Yes (nightly) | `wasm-bindgen-rayon` + SharedArrayBuffer + COOP/COEP |
| mmap-io | No | Use MemDriver (already exists in kithara-storage) |

## Phase 1: ABR Demo

**Demonstrates:** ABR algorithm making variant decisions based on throughput/buffer state.

**Changes:**
- New `crates/kithara-wasm` bin crate
- `#[wasm_bindgen]` wrappers over `AbrController` from `kithara-abr`
- `.cargo/config.toml` with `wasm-server-runner`
- `[profile.wasm-release]` in root `Cargo.toml`

**Core crate changes:** None. `kithara-abr` depends only on `tracing`, compiles on wasm32 as-is.

**Toolchain:** Stable Rust.

**Output:** Console.log with ABR decisions (variant selection, throughput estimates).

## Phase 2: Decode Demo

**Demonstrates:** Load audio file via File API → decode with symphonia in WASM → output PCM info.

**Approach:** Use symphonia directly via `Cursor<Vec<u8>>` as `Read + Seek`.
Bypasses kithara-stream (no rayon/tokio needed). Pure CPU-bound decoding.

**Changes in core:**
- `symphonia` feature `opt-simd`: verify wasm32 compilation, cfg-gate if needed
- Optional: demo `kithara-storage` MemDriver for in-WASM storage

**Dependencies added to kithara-wasm:** `symphonia`, `web-sys` (File API), `js-sys`.

**Toolchain:** Stable Rust.

**Output:** Console.log with format, codec, sample rate, channels, duration, sample count.

## Phase 3: Full HLS Player

**Demonstrates:** Full pipeline — HLS playlist → ABR → segment download → decode → playback via Web Audio API.

**Core crate cfg-gates:**

| Crate | Change |
|---|---|
| `kithara-storage` | `cfg(not(target_arch = "wasm32"))` for mmap-io; MemDriver default on wasm32 |
| `kithara-assets` | Use `MemAssetStore` / `MemStore` on wasm32 (already exists) |
| `kithara-stream` | ThreadPool: `wasm-bindgen-rayon` on wasm32 (nightly) |
| `kithara-net` | reqwest: no `rustls` feature on wasm32 (browser handles TLS) |
| `kithara-decode` | rodio `wasm-bindgen` feature for cpal WebAudio backend |
| `kithara-net` | `tokio::time` → `gloo-timers` on wasm32 (retry delays, timeouts) |

**Workspace Cargo.toml changes:**
- Target-conditional tokio features: `full` on native, `sync+macros+rt` on wasm32
- `wasm-bindgen`, `wasm-bindgen-futures`, `web-sys`, `js-sys`, `gloo-timers` in workspace deps

**Toolchain:** Nightly Rust (for wasm-bindgen-rayon).

**Build:**
```bash
RUSTFLAGS="-C target-feature=+atomics,+bulk-memory -C link-arg=--shared-memory" \
  trunk serve crates/kithara-wasm/index.html
```

**COOP/COEP headers** required for SharedArrayBuffer — configured via Trunk.toml.

## File Structure

```
crates/kithara-wasm/
├── Cargo.toml
├── src/
│   ├── main.rs        (entry point)
│   ├── abr.rs         (ABR demo — Phase 1)
│   ├── decode.rs      (decode demo — Phase 2)
│   └── player.rs      (HLS player — Phase 3)
├── index.html         (Trunk entry — Phase 3)
└── Trunk.toml         (Phase 3 config)

.cargo/config.toml     (wasm-server-runner for Phases 1-2)
```

## Build Profiles

```toml
# Root Cargo.toml
[profile.wasm-release]
inherits = "release"
opt-level = "z"
lto = "fat"
codegen-units = 1
```
