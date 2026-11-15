<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-beat.svg)](https://crates.io/crates/kithara-beat)
[![docs.rs](https://docs.rs/kithara-beat/badge.svg)](https://docs.rs/kithara-beat)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)

</div>

# kithara-beat

Beat and downbeat detection, with two detectors: a neural network ported from
the `beat_this` pipeline (CPJKU, ISMIR 2024) via `danigb/beat-this-rs` @
`089b509`, and a model-free DSP detector. Upstream code and model weights are
MIT-licensed; this crate keeps that attribution.

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
- `BeatBackend` — `Cpu` (default) or, with `cuda`, `Cuda { device_id }`.
- `BeatConfig` — backend, ONNX Runtime library path, peak threshold, and
  peak-picking widths. The defaults are what the golden fixtures are held to.
- `RawBeats { beats, downbeats }` — pooled output positions in seconds, sorted
  and deduplicated.
- `BeatDetector` — one window of mono audio in, beat marks out. Both
  backends implement it and a caller can supply its own; `mock` exposes
  `BeatDetectorMock`.
- `RawBeatGrid` / `BeatGridModel` — a beat grid as received and as
  validated; `BeatGridModel` is only built through `TryFrom<RawBeatGrid>`
  (serde included), and needs none of the detector code.

## Features

- `nn` — the neural-network detector on RTen (CPU).
- `dsp` — a detector that needs no model, on `kithara-dsp`.
- `cuda` — run the network through ONNX Runtime on CUDA. Supply the ONNX
  Runtime, CUDA, and cuDNN libraries; nothing is downloaded. The library path
  comes from `ORT_DYLIB_PATH` or `BeatConfig::builder().ort_library_path(...)`.
  Create one environment per process with
  `ort::init_from(config.ort_library_path())?.build()?` and pass it to each
  detector with `.environment(&environment)`. Mel extraction and peak picking
  stay on CPU.
- `embed-small-model`, `embed-full-model`, `embed-full-int8-model` — embed one
  model (`MEL_MODEL_BYTES`, `BEAT_MODEL_BYTES`) so mobile builds need no asset
  files. The build downloads it once into `KITHARA_BEAT_MODEL_CACHE` (default
  `~/.cargo/kithara-beat-models`) and checks a pinned SHA-256. The int8 model
  is quantized locally and must be placed in the cache by hand.

| feature | size | mean octave-folded error over 40 tracks |
|---|---|---|
| `embed-small-model` | 10.1 MB | 1.72 BPM |
| `embed-full-model` | 79 MB | 0.38 BPM |
| `embed-full-int8-model` | 22.6 MB | 0.35 BPM |

## Integration

Takes whole-track mono f32 PCM at 22 050 Hz and returns beat and downbeat
times in seconds. Decoding, downmix, resampling, and grid cleanup are done by
`kithara-analysis`.

See [crate contracts](https://github.com/zvuk/kithara/wiki/kithara-beat) for detailed contracts, invariants, and internals.
