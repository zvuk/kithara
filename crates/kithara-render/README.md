<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-render.svg)](https://crates.io/crates/kithara-render)
[![docs.rs](https://docs.rs/kithara-render/badge.svg)](https://docs.rs/kithara-render)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)

</div>

# kithara-render

The render stages of Kithara playback. On the producer side a decoded audio
source passes through its Warp renderer and effect chain, one render quantum at
a time, before the result enters the play output ring. On the audio thread a
deck mixes the tracks it holds, applying each command batch at the session
frame it is due on. Enable `mock` to drive a deck's audio-thread end from a
test in place of a `DeckMixer`.

## Usage

```rust,ignore
use kithara_render::WarpSource;

let stage = WarpSource::new(source, renderer, effects, drain, spec, pools);
// `stage` is itself an `AudioSource`: the play worker steps it like any
// decoded source and writes what it produces into the output ring.
```

## Key Types

<table>

<tr><th>Type</th><th>Role</th></tr>

<tr><td><code>WarpSource</code></td><td>Steps a decoded source through Warp and effects, keeps staged input across seeks and drains, and reports where a lane entering its plan starts decoding</td></tr>

<tr><td><code>rt::DeckMixer</code></td><td>The deck on the audio thread: takes its command batches at their frames and mixes its tracks, crossfades and declicks between them</td></tr>

<tr><td><code>rt::PlayerNode</code></td><td>The Firewheel node that hosts a deck in the output graph</td></tr>

<tr><td><code>rt::track::PcmConsumer</code></td><td>The half of a load a deck slot reads: the reader, the render it publishes, its playback rate and worker priority</td></tr>

<tr><td><code>bridge</code></td><td>The deck's channels, protocol, playback atomics, metrics and EQ control plane shared with the control side</td></tr>

<tr><td><code>CrossfadeSettings</code></td><td>The gain envelope of a crossfade between two tracks</td></tr>

</table>

## Features

| Feature | Effect |
| --- | --- |
| `stretch-signalsmith` | Signalsmith time-stretch backend in the Warp renderer |
| `stretch-bungee` | Bungee time-stretch backend in the Warp renderer |
| `stretch-glide` | Glide time-stretch backend in the Warp renderer |
| `mock` | `mock` module driving a deck's audio-thread end from tests |

## Integration

`kithara-play` builds one `WarpSource` per loaded track on its worker and
steps it from the decoder node, and hands each loaded track's `PcmConsumer` to
its deck. `kithara-host` places each deck's `PlayerNode` in the output graph.
The crate depends on the engine layer (`kithara-audio`, `kithara-effects`,
`kithara-warp`, `kithara-output`) and on nothing in the player.

See [crate contracts](https://github.com/zvuk/kithara/wiki/kithara-render) for detailed contracts, invariants, and internals.
