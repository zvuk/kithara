<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-beat.svg)](https://crates.io/crates/kithara-beat)
[![docs.rs](https://docs.rs/kithara-beat/badge.svg)](https://docs.rs/kithara-beat)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-beat

Beat / downbeat detection: pure-Rust NN inference. A port of the `beat_this`
pipeline (CPJKU, ISMIR 2024) via `danigb/beat-this-rs` @ `089b509`. Code and
model weights of both upstreams are MIT-licensed; this crate keeps that
attribution.

## Role

A leaf analysis crate: it takes whole-track mono f32 PCM at 22 050 Hz and
returns raw beat / downbeat positions in seconds. It owns no decoder, resampler,
or I/O — the consumer (`kithara-audio`) handles decode, downmix, resample, and
grid cleanup.

## Key types

- `BeatThis::try_from((mel_bytes, beat_bytes))` — load models from bytes
  (caller chooses embed vs file vs download).
- `BeatThis::analyze(&mono_22050)` — run the mel → inference → peak-pick pipeline.
- `RawBeats { beats, downbeats }` — output positions in seconds, sorted and
  deduplicated.

## Usage

```rust
use kithara_beat::{BeatThis, RawBeats};

let mut bt = BeatThis::try_from((mel_bytes, beat_bytes))?;
let raw: RawBeats = bt.analyze(&mono_22050)?;
```

## Features

- `embed-small-model` — exposes `MEL_MODEL_BYTES` / `BEAT_MODEL_BYTES`
  (`include_bytes!` of `models/mel_spectrogram.onnx`, 264 KB, and
  `models/beat_this_small.onnx`, 10.1 MB) so FFI/mobile builds need no asset
  plumbing. Off by default.

See [CONTEXT.md](CONTEXT.md) for detailed contracts, invariants, and internals.
