<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-beat.svg)](https://crates.io/crates/kithara-beat)
[![docs.rs](https://docs.rs/kithara-beat/badge.svg)](https://docs.rs/kithara-beat)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)

</div>

# kithara-beat

Beat / downbeat detection: pure-Rust NN inference. A port of the `beat_this`
pipeline (CPJKU, ISMIR 2024) via `danigb/beat-this-rs` @ `089b509`. Code and
model weights of both upstreams are MIT-licensed; this crate keeps that
attribution.

## Usage

```rust
use kithara_beat::{BeatThis, RawBeats};

let bt = BeatThis::builder()
    .mel_model(mel_bytes)
    .beat_model(beat_bytes)
    .pools(pools.clone())
    .build()?;
let raw: RawBeats = bt.analyze(&mono_22050)?;
```

## Key Types

- `BeatThis::builder()` — load models from bytes (caller chooses embed vs file
  vs download), inject a sample-capable pool facade, and pick the decoding policy.
- `BeatThis::analyze(&mono_22050)` — run the mel → inference → peak-pick pipeline.
- `BeatConfig` — peak threshold, max-pool half-width, dedup width. The defaults
  are the values the golden fixtures are held to; see CONTEXT.md before moving
  them.
- `RawBeats { beats, downbeats }` — pooled output positions in seconds, sorted
  and deduplicated.

## Features

- `embed-small-model` — exposes `MEL_MODEL_BYTES` / `BEAT_MODEL_BYTES`
  (`include_bytes!` of `models/mel_spectrogram.onnx`, 264 KB, and
  `models/beat_this_small.onnx`, 10.1 MB) so FFI/mobile builds need no asset
  plumbing. Off by default.

## Integration

A leaf analysis crate: it takes whole-track mono f32 PCM at 22 050 Hz and
returns raw beat / downbeat positions in seconds. It owns no decoder, resampler,
or I/O — the consumer (`kithara-analysis`) handles decode, downmix, resample, and
grid cleanup.

See [CONTEXT.md](CONTEXT.md) for detailed contracts, invariants, and internals.
