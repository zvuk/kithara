<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/kithara-wasm.svg)](https://crates.io/crates/kithara-wasm)
[![Downloads](https://img.shields.io/crates/d/kithara-wasm.svg)](https://crates.io/crates/kithara-wasm)
[![docs.rs](https://docs.rs/kithara-wasm/badge.svg)](https://docs.rs/kithara-wasm)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-wasm

WASM HLS audio player for browser environments. Provides JavaScript bindings via `wasm-bindgen` to the kithara audio pipeline, enabling in-browser HLS playback through AudioWorklet with shared memory.

## Usage

```js
import init, { setup, load_hls, WasmPlayer } from "kithara-wasm";

await init();
setup();

const [channels, sampleRate, durationMs] = await load_hls(hlsUrl);
const player = WasmPlayer.new();
player.play();
```

## Key Types

| Type | Role |
|------|------|
| `WasmPlayer` | Main player: `play()`, `pause()`, `seek()`, `fill_buffer()` |
| `setup()` | Initialize panic hook and tracing (call once before anything else) |
| `load_hls(url)` | Load HLS playlist; returns `[channels, sample_rate, duration_ms]` |
| `wasm_memory()` | Expose WASM memory as `SharedArrayBuffer` for AudioWorklet |
| `init_thread_pool(n)` | Initialize rayon thread pool (requires `threads` feature) |

## Architecture

- PCM ring buffer is shared with AudioWorklet via direct WASM memory access
- `WasmPlayer` state is stored in `thread_local!` to avoid `wasm-bindgen`'s `Rc<WasmRefCell<>>` wrapper (incompatible with shared memory / atomics)
- Backpressure: `fill_buffer()` only writes as many samples as the ring buffer can accept

## Features

| Feature | Default | Enables |
|---------|---------|---------|
| `threads` | yes | `wasm-bindgen-rayon` thread pool (requires `atomics` + `bulk-memory` target features) |

## Integration

Wraps `kithara-audio` (with `web-audio` feature), `kithara-hls`, and `kithara-stream` into a browser-friendly API. Designed for use with a JavaScript AudioWorklet that reads PCM samples from the shared ring buffer.
