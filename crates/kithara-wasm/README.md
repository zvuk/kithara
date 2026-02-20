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

WASM player bindings built on top of `kithara-play`.

## Usage

```js
import init, { setup, WasmPlayer } from "kithara-wasm";

await init();
setup();

const player = new WasmPlayer();
const index = player.add_track("https://example.com/track.mp3");
await player.select_track(index); // starts playback with crossfade
```

## Key API

- `WasmPlayer::add_track(url)`
- `WasmPlayer::select_track(index)`
- `WasmPlayer::play()`, `pause()`, `stop()`, `seek(ms)`
- `WasmPlayer::get_position_ms()`, `get_duration_ms()`
- `WasmPlayer::set_eq_gain(band, db)`, `reset_eq()`
- `setup()`
- `init_thread_pool(n)` (feature `threads`)

## Architecture

- Playback core is `kithara-play::PlayerImpl`
- Resource loading uses `kithara-play::Resource` / `ResourceConfig`
- Crossfade and EQ are handled in the shared player pipeline, not in JS-specific DSP

## Features

<table>
<tr><th>Feature</th><th>Default</th><th>Enables</th></tr>
<tr><td><code>threads</code></td><td>yes</td><td><code>wasm-bindgen-rayon</code> thread pool (requires <code>atomics</code> + <code>bulk-memory</code> target features)</td></tr>
</table>

## Browser requirements

The player uses `AudioWorklet` + shared memory paths and requires:

- secure context (`https:` or localhost)
- `SharedArrayBuffer`
- `crossOriginIsolated === true`

For production hosting, configure:

- `Cross-Origin-Opener-Policy: same-origin`
- `Cross-Origin-Embedder-Policy: require-corp`
- for Netlify/Cloudflare Pages, `_headers` file is included in this crate root

`Trunk.toml` already sets these headers for `trunk serve`.

For `gh-pages`, response headers are not reliably configurable for this case. Use one of:

- host the demo behind a proxy/CDN that injects COOP/COEP
- use `coi-serviceworker` fallback for demo-only scenarios

Minimal fallback bootstrap (demo only):

```html
<script src="./coi-serviceworker.js"></script>
```

At runtime, the demo checks these requirements and prints a clear error in the event log if isolation is missing.

## Integration

`kithara-wasm` is a wasm-bindgen wrapper around `kithara-play` so web and desktop follow the same playback logic.
