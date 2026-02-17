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

<table>
<tr><th>Type</th><th>Role</th></tr>
<tr><td><code>WasmPlayer</code></td><td>Main player: <code>play()</code>, <code>pause()</code>, <code>seek()</code>, <code>fill_buffer()</code></td></tr>
<tr><td><code>setup()</code></td><td>Initialize panic hook and tracing (call once before anything else)</td></tr>
<tr><td><code>load_hls(url)</code></td><td>Load HLS playlist; returns <code>[channels, sample_rate, duration_ms]</code></td></tr>
<tr><td><code>wasm_memory()</code></td><td>Expose WASM memory as <code>SharedArrayBuffer</code> for AudioWorklet</td></tr>
<tr><td><code>init_thread_pool(n)</code></td><td>Initialize rayon thread pool (requires <code>threads</code> feature)</td></tr>
</table>

## Architecture

- PCM ring buffer is shared with AudioWorklet via direct WASM memory access
- `WasmPlayer` state is stored in `thread_local!` to avoid `wasm-bindgen`'s `Rc<WasmRefCell<>>` wrapper (incompatible with shared memory / atomics)
- Backpressure: `fill_buffer()` only writes as many samples as the ring buffer can accept

## Features

<table>
<tr><th>Feature</th><th>Default</th><th>Enables</th></tr>
<tr><td><code>threads</code></td><td>yes</td><td><code>wasm-bindgen-rayon</code> thread pool (requires <code>atomics</code> + <code>bulk-memory</code> target features)</td></tr>
</table>

## Integration

Wraps `kithara-audio` (with `web-audio` feature), `kithara-hls`, and `kithara-stream` into a browser-friendly API. Designed for use with a JavaScript AudioWorklet that reads PCM samples from the shared ring buffer.
