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

let mut bt = BeatThis::builder()
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

- `embed-small-model`, `embed-full-model`, `embed-full-int8-model` — exactly one
  of these exposes `MEL_MODEL_BYTES` / `BEAT_MODEL_BYTES` / `BEAT_MODEL_TAG`
  (`include_bytes!` of `models/mel_spectrogram.onnx`, 264 KB, and the chosen
  beat model) so FFI/mobile builds need no asset plumbing. Off by default.

  | feature | model | size | mean octave-folded error over 40 tracks |
  |---|---|---|---|
  | `embed-small-model` | `small1` | 10.1 MB | 1.72 BPM |
  | `embed-full-model` | `final0` | 79 MB | 0.38 BPM |
  | `embed-full-int8-model` | `final0`, int8 | 22.6 MB | 0.35 BPM |

  The tree carries the small and mel models. The build fetches the full one
  into `$TMPDIR/kithara-beat-models`, or wherever `KITHARA_BEAT_MODEL_CACHE`
  points, and checks it against a pinned SHA-256. The int8 model is quantized
  from the full one: the build prints the command when it is missing.

## Integration

A leaf analysis crate: it takes whole-track mono f32 PCM at 22 050 Hz and
returns raw beat / downbeat positions in seconds. It owns no decoder, resampler,
or I/O — the consumer (`kithara-analysis`) handles decode, downmix, resample, and
grid cleanup.

See [CONTEXT.md](CONTEXT.md) for detailed contracts, invariants, and internals.
