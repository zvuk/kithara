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
- `BeatThis::analyze(&mono_22050)` — run the mel, inference and peak-pick pipeline.
- `BeatConfig` — peak threshold, max-pool half-width, dedup width. The defaults
  are the values the golden fixtures are held to; see [crate contracts](https://github.com/zvuk/kithara/wiki/kithara-beat) before moving
  them. `BeatThis::config()` reads the retained policy, and
  `BeatThis::apply_config_update(&mut self, update)` changes subsequent analyses.
- `RawBeats { beats, downbeats }` — pooled output positions in seconds, sorted
  and deduplicated.
- `BeatDetector` — the detector contract: one window of mono audio in, marks
  out. Both backends here implement it, and a caller can supply its own; the
  analysis pass drives a `dyn BeatDetector` and never names a backend. Errors
  arrive as `BeatDetectError`; the `mock` feature exposes `BeatDetectorMock`
  for a consumer's tests.
- `RawBeatGrid` / `BeatGridModel` — the served beat-grid contract: a document as
  it arrives, and the same document once its times, ordinals, bar anchors and
  meter have been checked. `BeatGridModel` is reachable only through
  `TryFrom<RawBeatGrid>`, which `serde` also routes deserialization through, so
  no unchecked grid exists. Model-only: no detector, no weights, no `nn`/`dsp`,
  so a server reading a stored grid carries nothing of the analyzer. A local
  pass reaches the same type through `kithara-analysis`, which states one grid
  per publication from its own beat artifact.

## Features

- `embed-small-model`, `embed-full-model`, `embed-full-int8-model` — exactly one
  of these exposes `MEL_MODEL_BYTES` / `BEAT_MODEL_BYTES` / `BEAT_MODEL_TAG`, so
  FFI/mobile builds need no asset plumbing. Off by default; the build fetches
  what the tree does not carry.

  | feature | size | mean octave-folded error over 40 tracks |
  |---|---|---|
  | `embed-small-model` | 10.1 MB | 1.72 BPM |
  | `embed-full-model` | 79 MB | 0.38 BPM |
  | `embed-full-int8-model` | 22.6 MB | 0.35 BPM |

## Integration

A leaf analysis crate: it takes whole-track mono f32 PCM at 22 050 Hz and
returns raw beat / downbeat positions in seconds. It owns no decoder, resampler,
or I/O — the consumer (`kithara-analysis`) handles decode, downmix, resample, and
grid cleanup.

See [crate contracts](https://github.com/zvuk/kithara/wiki/kithara-beat) for detailed contracts, invariants, and internals.
