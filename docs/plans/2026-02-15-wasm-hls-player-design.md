# Phase 3b: Full HLS Player in WASM — Design

## Goal

Full HLS audio player running in the browser via WebAssembly:
HLS playlist fetch, ABR variant selection, segment download, Symphonia decode,
PCM playback through Web Audio API. Multi-threaded via wasm-bindgen-rayon.

## Architecture

### Data flow (same as native, through existing crates)

```
reqwest(browser fetch) -> kithara-assets(MemStore) -> kithara-storage(MemResource/Ring)
  -> kithara-stream(Source/Read+Seek) -> kithara-decode(Symphonia) -> PcmChunk
  -> kithara-audio -> MemResource(Ring, PCM) -> AudioWorklet(JS) -> Web Audio API
```

### Thread model (wasm-bindgen-rayon)

| Thread | Role |
|--------|------|
| Main thread (browser) | UI events, JS glue, AudioContext + AudioWorklet creation |
| Rayon workers (Web Workers) | HLS fetch, decode, audio processing, PCM ring write |
| AudioWorklet thread | Reads PCM from ring buffer SharedArrayBuffer, feeds AudioContext |

## kithara-storage: MemDriver on ring buffer

Replace `Mutex<Vec<u8>>` with ring buffer backed by `slice_ring_buf` crate
and kithara-bufpool allocation.

### Internal changes

- `MemDriver` stores `Mutex<Pooled<Vec<u8>>>` (buffer from kithara-bufpool)
- `read_at`/`write_at` create temporary `SliceRbRefMut` from locked slice
- Capacity must be power-of-2 (bitmask indexing: `offset & (capacity - 1)`)

### Coverage invalidation on wrap-around

- `MemDriver` tracks `valid_window: Range<u64>` (current ring window)
- On write: if new data evicts old data (ring wraps), `valid_window.start` advances
- `MemCoverage::is_available(range)` checks intersection with valid_window
- `wait_range()` for evicted data sees gap, downloader re-fetches on demand

### Seek

1. Decoder seeks to offset X
2. `wait_range(X..X+N)` checks coverage
3. Data in ring window -> Ready, read works
4. Data evicted -> gap -> downloader demand request -> re-fetch -> Ready

### API unchanged

`ResourceExt` (read_at, write_at, wait_range, commit, status) stays the same.
All consumers (kithara-stream Source, kithara-hls, kithara-file) unchanged.

## Audio output

### Native (unchanged)

```
Audio<Stream<Hls>> -> rodio::Source (Iterator<Item=f32>) -> cpal -> OS audio
```

rodio 0.21.1 + cpal 0.16.0, no changes.

### WASM

```
Audio<Stream<Hls>> -> PCM ring buffer (MemResource) -> AudioWorklet(JS) -> WebAudio
```

#### Bridge: WASM <-> AudioWorklet

- WASM linear memory IS SharedArrayBuffer (with wasm-bindgen-rayon)
- Ring buffer in MemResource lives in shared memory
- JS gets SharedArrayBuffer view via `wasm_bindgen::memory()`
- AudioWorklet processor receives ring buffer offset + capacity via `port.postMessage`
- Synchronization: two atomics — `write_head` and `read_head`
  (JS: `Atomics.load`/`Atomics.store`)

#### Rust side (kithara-wasm)

- `#[wasm_bindgen]` struct `WasmPlayer` owns `Audio<Stream<Hls>>`
- Rayon worker: decode loop -> `audio.read(&mut pcm_buf)` -> `ring.write_at(write_head, &pcm_buf)` -> advance write_head
- Exports: `ring_buffer_ptr()`, `ring_buffer_len()`, `write_head_ptr()`, `read_head_ptr()`

#### JS side (AudioWorkletProcessor)

- `process(inputs, outputs)`: reads samples from SharedArrayBuffer view between read_head and write_head
- Advances read_head via `Atomics.store`
- If read_head == write_head -> silence (underrun)

#### cfg-gate in kithara-audio

- `#[cfg(feature = "rodio")]` — native audio output (unchanged)
- `#[cfg(feature = "web-audio")]` — WASM audio output (new module)

## Threading setup

### Build requirements

- Nightly Rust toolchain (for wasm32 target only, native stays stable)
- `RUSTFLAGS="-C target-feature=+atomics,+bulk-memory,+mutable-globals"`
- `-Z build-std=std,panic_abort`
- WASM memory = SharedArrayBuffer

### Initialization sequence

1. JS: `await wasm_bindgen()` — load WASM module
2. JS: `await initThreadPool(navigator.hardwareConcurrency)` — rayon Web Workers
3. JS: `new AudioWorkletNode(audioCtx, 'pcm-processor')` — register AudioWorklet
4. Rust: `WasmPlayer::new(url)` — start HLS pipeline via rayon

### Trunk configuration

`Trunk.toml` with COOP/COEP headers (required for SharedArrayBuffer):

```toml
[[headers]]
"Cross-Origin-Opener-Policy" = "same-origin"
"Cross-Origin-Embedder-Policy" = "require-corp"
```

### Cargo configuration

`.cargo/config.toml` (wasm32 target):

```toml
[target.wasm32-unknown-unknown]
rustflags = ["-C", "target-feature=+atomics,+bulk-memory,+mutable-globals"]
```

`Cargo.toml`:

```toml
[dependencies]
wasm-bindgen-rayon = { version = "1.2" }
rayon = { workspace = true, features = ["web_spin_lock"] }
```

## UI: minimal HTML player

### Files

- `crates/kithara-wasm/index.html` — player UI
- `crates/kithara-wasm/pcm-processor.js` — AudioWorklet processor
- `crates/kithara-wasm/src/lib.rs` — `#[wasm_bindgen]` exports

### HTML elements

- URL input + Load button
- Play / Pause / Stop buttons
- Seek slider (range input, updated from EventBus position)
- Variant selector (dropdown, populated from HLS playlist variants)
- Event log (scrollable div, HlsEvent/DecodeEvent)
- Status line (current variant, bitrate, buffer level, sample rate)

### JS -> Rust API (wasm_bindgen exports)

```
WasmPlayer::load(url: String)
WasmPlayer::play()
WasmPlayer::pause()
WasmPlayer::stop()
WasmPlayer::seek(position_ms: f64)
WasmPlayer::set_variant(index: usize)
WasmPlayer::get_variants() -> JsValue
WasmPlayer::get_position_ms() -> f64
WasmPlayer::get_duration_ms() -> f64
```

### Events: Rust -> JS

Polling via requestAnimationFrame: JS calls `get_position_ms()` to update slider.

### Style

Minimal dark CSS (same as current decode demo), no frameworks.

## New dependencies

| Dependency | Version | Purpose |
|---|---|---|
| `slice_ring_buf` | `0.3` | Ring buffer for MemDriver |
| `wasm-bindgen-rayon` | `1.2` | Rayon -> Web Workers |
| `trunk` | CLI tool | Build + dev server with COOP/COEP |

## Crate changes

| Crate | Change |
|---|---|
| kithara-storage | MemDriver: Vec -> SliceRbRefMut on Pooled<Vec>, coverage invalidation on wrap |
| kithara-audio | New `web-audio` feature: PCM ring buffer output instead of rodio |
| kithara-wasm | Rewrite: lib.rs with WasmPlayer, pcm-processor.js, new index.html |

### Unchanged crates

kithara-stream, kithara-hls, kithara-decode, kithara-net, kithara-assets,
kithara-bufpool — no changes needed.

## Risks

1. **Nightly toolchain** — only for wasm32 target, native stays stable
2. **SharedArrayBuffer memory** — cannot resize after creation, must set max at build time
3. **AudioWorklet TextEncoder** — known wasm-bindgen limitation, AudioWorklet has restricted API; our JS processor is pure JS (no wasm-bindgen in worklet), so not affected
4. **CORS** — external HLS sources must serve CORS headers; user responsibility
